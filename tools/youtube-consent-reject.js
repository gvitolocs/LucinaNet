// Test harness only: answers Google's cookie dialog with "Reject all" (the owner's privacy-preserving choice), as he would
// once by hand in a fresh profile. YouTube pauses the video while the dialog is open. Nothing else is touched.
(() => {
  if (location.host !== "www.youtube.com" || window.__lucinaConsent) return;
  window.__lucinaConsent = true;
  const labels = ["Reject all", "Afvis alle"];
  const timer = setInterval(() => {
    for (const button of document.querySelectorAll("button, tp-yt-paper-button, yt-button-shape button")) {
      const text = (button.innerText || button.textContent || "").trim();
      if (labels.includes(text)) {
        button.click();
        console.log(`ytprobe consent: clicked "${text}"`);
        clearInterval(timer);
        return;
      }
    }
  }, 500);
  setTimeout(() => clearInterval(timer), 30000);
})();
