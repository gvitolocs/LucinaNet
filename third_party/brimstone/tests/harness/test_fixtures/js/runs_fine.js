/*---
description: >
    Finishes at once, and must still run after a test before it timed out
flags: [raw]
---*/
var sum = 0;
for (var i = 0; i < 100; i++) {
    sum += i;
}
if (sum !== 4950) {
    throw new Error("wrong sum: " + sum);
}
