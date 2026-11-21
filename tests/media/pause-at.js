// Pause the page's video at a fixed time, so every run snapshots the same decoded frame.
addEventListener("load", () => {
  const v = document.querySelector("video");
  if (!v) {
    console.log("PAUSE no video");
    return;
  }
  const seek = () => {
    v.pause();
    v.currentTime = 1.0;
    console.log("PAUSE seeking to " + v.currentTime);
  };
  if (v.readyState >= 1) seek();
  else v.addEventListener("loadeddata", seek, { once: true });
  v.addEventListener("seeked", () => console.log("PAUSE seeked to " + v.currentTime + " paused=" + v.paused), { once: true });
});
