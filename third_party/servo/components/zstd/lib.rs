/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pure-Rust zstd decoding for `Content-Encoding: zstd`, replacing
//! async-compression's C-libzstd decoder. Built on ruzstd's `FrameDecoder`,
//! with the frame-header policy of RFC 9659 (8 MiB window) in `window`.

mod window;

pub use window::{Header, MAX_WINDOW, Refused};

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use ruzstd::decoding::FrameDecoder;
use tokio::io::{AsyncBufRead, AsyncRead, ReadBuf};

/// A compressed block is at most 128 KiB, its header 3 bytes, a frame header
/// at most `MAX_HEADER` bytes and a checksum 4 bytes: never buffer more input
/// than this.
const MAX_INPUT: usize = 128 * 1024 + 3 + window::MAX_HEADER + 4;

fn invalid_data(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn unexpected_eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "zstd stream did not finish")
}

fn refused_error(r: Refused) -> io::Error {
    let msg = match r {
        Refused::Magic => "not a zstd frame",
        Refused::Reserved => "reserved bit set in frame header descriptor",
        Refused::Dictionary => "frame requires a dictionary",
        Refused::Window => "frame window larger than 8 MiB",
    };
    invalid_data(msg)
}

/// Synchronous decoding core. Feed compressed bytes with `decode`, collect
/// decompressed bytes from the output slice; `eof` says the input will not
/// grow anymore.
pub struct Decoder {
    /// Buffered compressed input, never longer than MAX_INPUT.
    buf: Vec<u8>,
    /// The frame being decoded, if any.
    frame: Option<FrameDecoder>,
    /// Whether the window limit was already given to the frame decoder.
    max_window_set: bool,
    /// Bytes written for the current frame.
    frame_written: u64,
    /// Content size the current frame's header declared, if any.
    declared_size: Option<u64>,
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            buf: Vec::new(),
            frame: None,
            max_window_set: false,
            frame_written: 0,
            declared_size: None,
        }
    }

    /// Whether the stream is complete: between frames with nothing buffered.
    pub fn is_finished(&self) -> bool {
        self.frame.is_none() && self.buf.is_empty()
    }

    /// How many compressed bytes are buffered internally (bounded by MAX_INPUT).
    pub fn buffered_input(&self) -> usize {
        self.buf.len()
    }

    /// Decode as much of `input` as possible into `output`.
    /// Returns (consumed, written). With `eof` set, a stream that ends
    /// mid-frame is an `UnexpectedEof` error.
    pub fn decode(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        eof: bool,
    ) -> io::Result<(usize, usize)> {
        let mut consumed = 0usize;
        let mut written = 0usize;
        loop {
            // Buffer as much of the remaining input as fits.
            let take = (MAX_INPUT - self.buf.len()).min(input.len() - consumed);
            self.buf.extend_from_slice(&input[consumed..consumed + take]);
            consumed += take;

            if self.frame.is_none() {
                if self.buf.is_empty() {
                    if eof {
                        return Ok((consumed, written));
                    }
                    break;
                }
                match window::frame_header(&self.buf) {
                    Err(r) => return Err(refused_error(r)),
                    Ok(Header::NeedMore { .. }) => {
                        if eof {
                            return Err(unexpected_eof());
                        }
                        break;
                    }
                    Ok(Header::Skippable { data }) => {
                        let need = window::SKIPPABLE_HEADER + data as usize;
                        if self.buf.len() < need {
                            if eof {
                                return Err(unexpected_eof());
                            }
                            break;
                        }
                        self.buf.drain(..need);
                        continue;
                    }
                    Ok(Header::Frame { .. }) => {
                        // The content size the header declares, if any.
                        let descriptor = self.buf[4];
                        let fcs = window::fcs_len(descriptor);
                        let did = window::did_len(descriptor);
                        let wd_len = if descriptor & 0x20 != 0 { 0 } else { 1 };
                        let declared = if fcs > 0 {
                            Some(window::content_size(&self.buf, 5 + wd_len + did, fcs))
                        } else {
                            None
                        };
                        let frame = self.frame.get_or_insert_with(FrameDecoder::new);
                        if !self.max_window_set {
                            frame.set_max_window_size(MAX_WINDOW);
                            self.max_window_set = true;
                        }
                        frame
                            .reset(&self.buf[..])
                            .map_err(|_| invalid_data("invalid zstd frame header"))?;
                        let header_len = frame.bytes_read_from_source() as usize;
                        self.buf.drain(..header_len);
                        self.frame_written = 0;
                        self.declared_size = declared;
                        continue;
                    }
                }
            }

            let out = &mut output[written..];
            if out.is_empty() {
                break;
            }
            let frame = match self.frame.as_mut() {
                Some(f) => f,
                None => break,
            };
            let (read, w) = frame
                .decode_from_to(&self.buf, out)
                .map_err(|_| invalid_data("invalid zstd block"))?;
            // ruzstd reports 4 bytes read for a pending checksum even when the
            // source held fewer; that report is phantom, drain nothing then.
            let read = if read > self.buf.len() { 0 } else { read };
            self.buf.drain(..read);
            written += w;
            self.frame_written += w as u64;

            if frame.is_finished() {
                if read == 0 && w == 0 {
                    // Frame complete: verify checksum and declared content size.
                    let frame = self.frame.take();
                    if let Some(frame) = frame {
                        if let (Some(from_data), Some(calculated)) = (
                            frame.get_checksum_from_data(),
                            frame.get_calculated_checksum(),
                        ) {
                            if from_data != calculated {
                                return Err(invalid_data("zstd checksum mismatch"));
                            }
                        }
                    }
                    if let Some(declared) = self.declared_size {
                        if self.frame_written != declared {
                            return Err(invalid_data("zstd content size mismatch"));
                        }
                    }
                    continue;
                }
            } else if read == 0 && w == 0 {
                // No full block buffered: need more input.
                if self.buf.len() >= MAX_INPUT {
                    return Err(invalid_data("zstd block larger than 128 KiB"));
                }
                if eof {
                    return Err(unexpected_eof());
                }
                break;
            }
        }
        Ok((consumed, written))
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

pin_project_lite::pin_project! {
    /// `tokio::io::AsyncRead` over a zstd-compressed `AsyncBufRead`, same
    /// shape as async-compression's `tokio::bufread::ZstdDecoder`.
    pub struct ZstdDecoder<R> {
        #[pin]
        reader: R,
        core: Decoder,
        eof: bool,
    }
}

impl<R: AsyncBufRead> ZstdDecoder<R> {
    pub fn new(reader: R) -> Self {
        ZstdDecoder {
            reader,
            core: Decoder::new(),
            eof: false,
        }
    }
}

impl<R: AsyncBufRead> AsyncRead for ZstdDecoder<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut this = self.project();
        // Nothing can be written: return at once instead of buffering input for ever.
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if *this.eof && this.core.is_finished() {
                return Poll::Ready(Ok(()));
            }
            let input: &[u8] = match this.reader.as_mut().poll_fill_buf(cx) {
                Poll::Ready(Ok(data)) => {
                    if data.is_empty() {
                        *this.eof = true;
                        &[]
                    } else {
                        data
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };
            let mut tmp = [0u8; 8192];
            let out_len = tmp.len().min(buf.remaining());
            match this.core.decode(input, &mut tmp[..out_len], *this.eof) {
                Err(e) => return Poll::Ready(Err(e)),
                Ok((consumed, written)) => {
                    this.reader.as_mut().consume(consumed);
                    buf.put_slice(&tmp[..written]);
                    if written > 0 {
                        return Poll::Ready(Ok(()));
                    }
                    if consumed == 0 && *this.eof && !this.core.is_finished() {
                        return Poll::Ready(Err(unexpected_eof()));
                    }
                }
            }
        }
    }
}
