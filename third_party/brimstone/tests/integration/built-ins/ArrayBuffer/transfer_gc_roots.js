/*---
description: >
  ArrayBuffer.prototype.transfer and transferToFixedLength keep the source and the new buffer
  rooted while they allocate (ArrayBufferCopyAndDetach checks the detach key, then allocates the new
  buffer and copies or hands over the data block). A collection in the middle of it moves both, so a
  transfer that used an address from before the move would hand back a buffer whose bytes are not
  the source's, or detach the wrong object. Run under BS_GC_STRESS to make every allocation collect.
---*/

const kept = [];
for (let round = 0; round < 300; round++) {
  const length = 8 + (round % 24);
  const source = new ArrayBuffer(length, round % 3 === 0 ? { maxByteLength: 64 } : undefined);
  const bytes = new Uint8Array(source);
  for (let i = 0; i < length; i++) bytes[i] = (round + i) & 0xff;

  // Same length (the data block is handed over), longer and shorter (a copy), and fixed
  const newLength = [undefined, length + 5, length - 3][round % 3];
  const target = round % 2 === 0 ? source.transfer(newLength) : source.transferToFixedLength(newLength);

  assert.sameValue(source.detached, true);
  assert.sameValue(source.byteLength, 0);
  assert.sameValue(bytes.length, 0);

  const expectedLength = newLength === undefined ? length : newLength;
  assert.sameValue(target.byteLength, expectedLength);
  assert.sameValue(target.resizable, round % 2 === 0 && round % 3 === 0);
  kept.push([target, round, length]);

  // Garbage between rounds, so that the kept buffers move many times
  for (let i = 0; i < 20; i++) kept.push({ i, s: "x" + i });
  if (kept.length > 400) kept.splice(0, 21);
}

for (const entry of kept) {
  if (!Array.isArray(entry)) continue;
  const [target, round, length] = entry;
  const view = new Uint8Array(target);
  const copied = Math.min(length, view.length);
  for (let i = 0; i < copied; i++) assert.sameValue(view[i], (round + i) & 0xff);
  for (let i = copied; i < view.length; i++) assert.sameValue(view[i], 0);
}
