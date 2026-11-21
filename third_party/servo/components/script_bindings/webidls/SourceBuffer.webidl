/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
/*
 * The origin of this IDL file is
 * https://www.w3.org/TR/media-source-2/#sourcebuffer
 */

[Exposed=Window]
interface SourceBuffer : EventTarget {
  [SetterThrows] attribute AppendMode mode;
  readonly attribute boolean updating;
  [Throws] readonly attribute TimeRanges buffered;

  [SetterThrows] attribute double timestampOffset;
  [SetterThrows] attribute double appendWindowStart;
  [SetterThrows] attribute unrestricted double appendWindowEnd;

  attribute EventHandler onupdatestart;
  attribute EventHandler onupdate;
  attribute EventHandler onupdateend;
  attribute EventHandler onerror;
  attribute EventHandler onabort;

  [Throws] undefined appendBuffer(BufferSource data);
  [Throws] undefined abort();
  [Throws] undefined changeType(DOMString type);
  [Throws] undefined remove(double start, unrestricted double end);
};

// audioTracks, videoTracks and textTracks are left out for now.

enum AppendMode {
  "segments",
  "sequence"
};
