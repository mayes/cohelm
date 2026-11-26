# The Helper drives the browser over a private pipe, not a debugging port

The **Helper** starts the browser itself with `--remote-debugging-pipe` and its own profile folder, and holds the only ends of that pipe. The browser opens no debugging port, so no other program on the computer, and no web page, can drive it or read the person's signed-in sessions through DevTools. A local port would have been quicker to build but lets any local program take over the browser, which goes against keeping logins safe on the device.

Chrome DevTools MCP can only reach a browser over a WebSocket, so the Helper relays one: it listens on loopback at a path holding a fresh 256-bit secret, refuses any request with an `Origin` header (web pages always send one), and passes messages to and from the pipe. The secret reaches Chrome DevTools MCP in a config file inside a folder only the person's account can read, not on its command line, where other programs could see it. One client is relayed at a time; when it goes, the Helper detaches its sessions and turns off its auto-attach, so new tabs never wait on a client that's gone.

On Mac and Linux the pipe is file descriptors 3 and 4. On Windows the Helper passes the two pipe handles with `--remote-debugging-io-pipes=<read>,<write>`.

## Consequences

- The Helper is the browser's parent: quitting the browser stops the Helper, and stopping the Helper closes the browser (it asks first, then stops it after five seconds). Packaging starts the Helper as the app, and the Helper starts Chromium.
- Cohelm always uses its own profile folder. Chromium ignores debugging switches on the default Chrome profile anyway.
- The relay is still a loopback socket. A program running as the same person that reads the config file in the short time it exists could connect. That program could already read the profile folder, so this is accepted for 0.1; recheck before 1.0, for example by giving Chrome DevTools MCP the pipe directly.
- On Windows the pipe handles are inheritable while the browser starts, so other handles the Helper marks inheritable could reach Chromium too. The Helper marks none. Limiting the inherited handles to the two pipe ends needs Windows-only process attributes and can come later.
- The Windows path is only compile-checked in CI. The release smoke test on the Windows build machine has to confirm it.
