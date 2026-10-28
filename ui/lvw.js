// Thin wrappers over the Tauri globals so pages work without a bundler.
window.lvw = {
  invoke: (cmd, args) => window.__TAURI__.core.invoke(cmd, args),
  listen: (name, cb) => window.__TAURI__.event.listen(name, (e) => cb(e.payload)),
};
