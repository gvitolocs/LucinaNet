// LucinaNet profile test, second run: what the profile gave back after the first run was killed.
(function () {
  let ls = null;
  try { ls = localStorage.getItem("lucina"); } catch (e) { ls = "error " + e; }
  if (typeof indexedDB === "undefined") { console.log("profile-test: got cookie=" + document.cookie + " ls=" + ls + " idb=unavailable"); return; }
  const open = indexedDB.open("lucina-profile", 1);
  open.onupgradeneeded = () => open.result.createObjectStore("kv");
  open.onsuccess = () => {
    const get = open.result.transaction("kv", "readonly").objectStore("kv").get("lucina");
    get.onsuccess = () => console.log("profile-test: got cookie=" + document.cookie + " ls=" + ls + " idb=" + get.result);
    get.onerror = () => console.log("profile-test: got cookie=" + document.cookie + " ls=" + ls + " idb=error");
  };
  open.onerror = () => console.log("profile-test: got cookie=" + document.cookie + " ls=" + ls + " idb=open-error");
})();
