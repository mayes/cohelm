# The Helper is the app, and the browser ships inside it unchanged

The person starts Cohelm, and what starts is the **Helper**. It finds the browser, the built-in extension, Chrome DevTools MCP and Node beside itself, and starts the browser over the private pipe (ADR 0005). The browser build is copied in as it comes out of the build, with its own bundle and signature, so packaging never edits Chromium's files.

On a Mac:

```
Cohelm.app/Contents/MacOS/Cohelm         the Helper (a background app, no Dock icon)
Cohelm.app/Contents/Resources/
  Cohelm Browser.app                     the browser, unchanged
  extension/                             Cohelm's built-in extension
  devtools-mcp/                          Chrome DevTools MCP
  node/bin/node                          Node, to run it
```

On Windows, installed for the current user in `%LOCALAPPDATA%\Programs\Cohelm`:

```
Cohelm.exe      the Helper (no console window)
browser\        the browser, unchanged
extension\      Cohelm's built-in extension
devtools-mcp\   Chrome DevTools MCP
node\node.exe   Node, to run it
```

The Mac app comes in a disk image. Windows gets an Inno Setup installer that needs no admin rights, adds a Start menu entry and starts Cohelm when it finishes. Uninstalling keeps the profile, as browsers do.

Node and Chrome DevTools MCP are downloaded at pinned versions and checked against pinned checksums (`browser/packaging/runtime.env`), so a release always ships the parts it was tested with and needs nothing installed on the person's computer. Chrome DevTools MCP has no dependencies of its own, so the npm tarball is all it needs.

The built-in extension is loaded with `--load-extension`. Branded Chrome removed that switch in 137, but Chromium and ungoogled-chromium keep it. Its ID is fixed by the public key in its manifest. Chrome DevTools MCP refuses `chrome-extension:` pages unless started with `--categoryExtensions`, which the Helper never passes, so an **Agent** can't drive Cohelm's own pages, such as a **Pause**.

## Considered options

- **Patch Chromium to start the Helper.** Every Chromium update would need the patch rebased, and the browser would have to be re-signed. Packaging does the same job without touching Chromium.
- **Rely on Node already being installed.** Many people don't have it, and the version would vary. Shipping Node makes the download bigger, but less than Chromium itself does.
- **A system-wide Windows installer.** Needs admin rights for no benefit; Chrome and most browsers install per user.

## Consequences

- An installed Helper logs to `Helper.log` beside the profile folder, since it has no terminal. `COHELM_BROWSER`, `COHELM_EXTENSION_DIR`, `COHELM_DEVTOOLS_MCP` and `COHELM_PROFILE_DIR` still override each part, for development.
- Starting Cohelm a second time while it runs hands the pages to the running browser through Chromium's own single-instance check, and no second Helper starts.
- The beta is unsigned (ADR 0003). The outer Mac app is signed ad hoc so its parts hold together, and the browser keeps its own signature. Gatekeeper and SmartScreen will warn until signing is set up.
- CI packages Chrome for Testing as a stand-in for the browser build, installs it the way a person would, and runs the installed-app smoke test on Mac and Windows. Nothing it builds is uploaded. The same test runs on each real build on the build machines before release.
- Known gaps for 0.1:
  - The inner browser still names itself Chromium in the Dock, menus and taskbar. Naming it Cohelm needs ungoogled-chromium's branding settings in the build.
  - Opening Cohelm.app again on a Mac while it runs does nothing visible, because the background Helper is already running. Clicking the browser in the Dock brings it forward.
  - Pinning the inner browser to the Dock or taskbar would start it without the Helper. Agents would then have nothing to connect to until Cohelm is started again.
