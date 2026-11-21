/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Saving a navigation response to a file instead of showing it.
//!
//! The body keeps coming from the fetch that the navigation already started, so a POST or a
//! one-time link is not fetched twice and cookies, authentication and redirects stay as they
//! were. It arrives on the fetch thread, which serves every fetch of the process, so the file is
//! written by a thread of its own: a slow disk then holds up neither other fetches nor script.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, bounded, never, select, unbounded};
use embedder_traits::{DownloadId, EmbedderMsg, ScriptToEmbedderChan};
use http::header;
use net_traits::download::{download_directory, download_file_name, reserve_download_file};
use net_traits::request::RequestId;
use net_traits::{
    CoreResourceThread, FetchResponseMsg, Metadata, NetworkError, cancel_async_fetch,
};
use servo_base::generic_channel::GenericCallback;
use servo_base::id::WebViewId;

/// The embedder is told about progress at most this often (four times a second), so that a fast
/// download does not flood it with messages.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

enum WriterMessage {
    Chunk(Bytes),
    Done(Result<(), NetworkError>),
}

/// The fetch thread's end of a download: it hands the rest of the response to the writer thread.
#[derive(Clone)]
pub(crate) struct DownloadWriter {
    sender: Sender<WriterMessage>,
}

/// What the writer thread needs to know about the download, taken from the response headers.
struct DownloadRequest {
    webview_id: WebViewId,
    request_id: RequestId,
    id: DownloadId,
    file_name: String,
    total: Option<u64>,
    /// Why the response will not be saved, for an HTTP error status: like Chromium, the error
    /// page of a failed download is not saved as if it were the file.
    server_error: Option<String>,
    embedder_sender: ScriptToEmbedderChan,
    core_resource_thread: CoreResourceThread,
}

impl DownloadWriter {
    /// Starts saving the response described by `metadata`. The body chunks that follow must be
    /// passed to [`DownloadWriter::forward`].
    pub(crate) fn start(
        webview_id: WebViewId,
        request_id: RequestId,
        metadata: &Metadata,
        download_attribute: Option<&str>,
        embedder_sender: ScriptToEmbedderChan,
        core_resource_thread: CoreResourceThread,
    ) -> DownloadWriter {
        let headers = metadata.headers.as_deref();
        let file_name = download_file_name(headers, download_attribute, &metadata.final_url);
        // The length is of the encoded body; a compressed one is decoded before it gets here, so
        // its length says nothing about how many bytes will be written.
        let total = headers
            .filter(|headers| !headers.contains_key(header::CONTENT_ENCODING))
            .and_then(|headers| headers.get(header::CONTENT_LENGTH))
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        let is_http = matches!(metadata.final_url.scheme(), "http" | "https");
        let server_error = (is_http && !metadata.status.is_success()).then(|| {
            format!(
                "The server answered {} {}",
                metadata.status.raw_code(),
                String::from_utf8_lossy(metadata.status.message())
            )
        });

        let (sender, receiver) = unbounded();
        let download = DownloadRequest {
            webview_id,
            request_id,
            id: DownloadId(request_id.0),
            file_name,
            total,
            server_error,
            embedder_sender,
            core_resource_thread,
        };
        let spawned = thread::Builder::new()
            .name("Download".to_owned())
            .spawn(move || download.write(receiver));
        if let Err(error) = spawned {
            warn!("Could not start a thread for a download: {error}");
        }
        DownloadWriter { sender }
    }

    /// Passes on a message of the download's fetch. When the writer has already stopped (it was
    /// cancelled or could not write), the message has nowhere to go and is dropped.
    pub(crate) fn forward(&self, message: FetchResponseMsg) {
        let message = match message {
            FetchResponseMsg::ProcessResponseChunk(_, chunk) => WriterMessage::Chunk(chunk),
            FetchResponseMsg::ProcessResponseEOF(_, result, _) => WriterMessage::Done(result),
            _ => return,
        };
        let _ = self.sender.send(message);
    }
}

impl DownloadRequest {
    fn send(&self, message: EmbedderMsg) {
        if let Err(error) = self.embedder_sender.send(message) {
            warn!(
                "Could not tell the embedder about download {}: {error:?}",
                self.id
            );
        }
    }

    fn cancel_fetch(&self) {
        cancel_async_fetch(vec![self.request_id], &self.core_resource_thread);
    }

    fn finish(&self, result: Result<PathBuf, String>) {
        self.send(EmbedderMsg::NotifyDownloadFinished(
            self.webview_id,
            self.id,
            result,
        ));
    }

    /// The writer thread: tells the embedder the download started, writes the body to
    /// `<name>.part` and renames it once it is complete. On any failure or cancel the `.part`
    /// file is deleted, so a half-written file is never left looking like a whole one.
    fn write(self, receiver: Receiver<WriterMessage>) {
        let request_id = self.request_id;
        let core_resource_thread = self.core_resource_thread.clone();
        let (cancel_sender, cancel_receiver) = bounded(1);
        let cancel = GenericCallback::new(move |_| {
            cancel_async_fetch(vec![request_id], &core_resource_thread);
            // The fetch may be stalled and send nothing more, so wake the writer directly.
            let _ = cancel_sender.try_send(());
        });
        let cancel = match cancel {
            Ok(cancel) => cancel,
            Err(error) => {
                warn!("Could not make a cancel callback for a download: {error:?}");
                return self.cancel_fetch();
            },
        };

        let directory = download_directory();
        let (final_path, part_path, file) = match reserve_download_file(&directory, &self.file_name)
        {
            Ok(reserved) => reserved,
            Err(error) => {
                self.cancel_fetch();
                self.send(EmbedderMsg::NotifyDownloadStarted(
                    self.webview_id,
                    self.id,
                    self.file_name.clone(),
                    self.total,
                    cancel,
                ));
                return self.finish(Err(format!(
                    "Could not create a file in {}: {error}",
                    directory.display()
                )));
            },
        };
        let shown_name = final_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file_name.clone());
        self.send(EmbedderMsg::NotifyDownloadStarted(
            self.webview_id,
            self.id,
            shown_name,
            self.total,
            cancel,
        ));

        if let Some(server_error) = &self.server_error {
            self.cancel_fetch();
            drop(file);
            remove_part_file(&part_path);
            return self.finish(Err(server_error.clone()));
        }

        let result = self
            .receive_body(&receiver, cancel_receiver, BufWriter::new(file), &part_path)
            .and_then(|()| {
                fs::rename(&part_path, &final_path)
                    .map_err(|error| format!("Could not rename {}: {error}", part_path.display()))
            });
        match result {
            Ok(()) => self.finish(Ok(final_path)),
            Err(reason) => {
                remove_part_file(&part_path);
                self.finish(Err(reason));
            },
        }
    }

    /// Writes chunks until the fetch ends, and makes sure the bytes are on disk before the file
    /// gets its real name.
    fn receive_body(
        &self,
        receiver: &Receiver<WriterMessage>,
        mut cancel_receiver: Receiver<()>,
        mut file: BufWriter<File>,
        part_path: &Path,
    ) -> Result<(), String> {
        let write_error =
            |error: std::io::Error| format!("Could not write {}: {error}", part_path.display());
        let mut received: u64 = 0;
        let mut last_progress: Option<Instant> = None;
        loop {
            let message = select! {
                recv(receiver) -> message => message,
                recv(cancel_receiver) -> cancelled => {
                    if cancelled.is_ok() {
                        return Err("Cancelled".to_owned());
                    }
                    // The embedder let go of the cancel callback (its WebView closed); like a
                    // browser closing a tab, that does not stop the download.
                    cancel_receiver = never();
                    continue;
                },
            };
            match message {
                Ok(WriterMessage::Chunk(chunk)) => {
                    if let Err(error) = file.write_all(&chunk) {
                        self.cancel_fetch();
                        return Err(write_error(error));
                    }
                    received += chunk.len() as u64;
                    if last_progress.is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL) {
                        last_progress = Some(Instant::now());
                        self.send(EmbedderMsg::NotifyDownloadProgress(
                            self.webview_id,
                            self.id,
                            received,
                            self.total,
                        ));
                    }
                },
                Ok(WriterMessage::Done(Ok(()))) => break,
                Ok(WriterMessage::Done(Err(NetworkError::LoadCancelled))) => {
                    return Err("Cancelled".to_owned());
                },
                Ok(WriterMessage::Done(Err(error))) => {
                    return Err(format!("Network error: {error:?}"));
                },
                // The fetch thread dropped the fetch without an end (the process is shutting down).
                Err(_) => return Err("The download was interrupted".to_owned()),
            }
        }
        let file = file
            .into_inner()
            .map_err(|error| write_error(error.into_error()))?;
        file.sync_all().map_err(write_error)?;
        // The last chunk may have come within the progress interval; the embedder should see the
        // whole size before it hears that the download is done.
        self.send(EmbedderMsg::NotifyDownloadProgress(
            self.webview_id,
            self.id,
            received,
            self.total,
        ));
        Ok(())
    }
}

fn remove_part_file(part_path: &Path) {
    if let Err(error) = fs::remove_file(part_path) {
        warn!("Could not delete {}: {error}", part_path.display());
    }
}
