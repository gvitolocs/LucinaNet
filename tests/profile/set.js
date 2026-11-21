// LucinaNet profile test, first run: what a page leaves in the profile (cookie from the server, localStorage, IndexedDB).
(function () {
  try { localStorage.setItem("lucina", "ls-" + location.host); } catch (e) { console.log("profile-test: ls error " + e); }
  if (typeof indexedDB === "undefined") { console.log("profile-test: set done (no IndexedDB), cookie=" + document.cookie); return; }
  const open = indexedDB.open("lucina-profile", 1);
  open.onupgradeneeded = () => open.result.createObjectStore("kv");
  open.onerror = () => console.log("profile-test: idb open error " + open.error);
  open.onsuccess = () => {
    const tx = open.result.transaction("kv", "readwrite");
    tx.objectStore("kv").put("idb-" + location.host, "lucina");
    tx.oncomplete = () => console.log("profile-test: set done, cookie=" + document.cookie);
    tx.onerror = () => console.log("profile-test: idb put error " + tx.error);
  };
})();
