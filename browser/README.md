# Cohelm browser build

Cohelm's browser is [ungoogled-chromium](https://github.com/ungoogled-software/ungoogled-chromium) with a thin Cohelm layer on top. The release it's built on is pinned in [`ungoogled-chromium.env`](ungoogled-chromium.env).

Full builds take hours and run on the project's self-hosted build machines (ADR 0003). Nothing here runs on GitHub's hosted runners except the script tests.

## What Cohelm adds

| Layer | Where | What it does |
| --- | --- | --- |
| Patches | [`patches/series`](patches/series) | Cohelm's own Chromium patches, applied after ungoogled-chromium's. Empty for now, on purpose. |
| GN flags | [`flags.gn`](flags.gn) | Repeats the privacy flags Cohelm relies on, after the platform repo's flags. |
| Packaging | [`packaging/`](packaging) | Makes the Helper the app, with the browser, the built-in extension, Chrome DevTools MCP and Node inside it (ADR 0006). The Helper starts Chromium in Cohelm's own profile folder over a private DevTools pipe (ADR 0005). |

The patch set stays small so each Chromium update is a quick rebase. Anything packaging can do, packaging does. A patch is added only when there's no other way, it makes one change, and its header says why it exists.

### Identity

Cohelm doesn't change the user agent or client hints. Sites see the same strings as stock Chromium of the same version, with no Cohelm name or ID. The Helper's browser test checks this through an Agent, and the release smoke test checks it on each full build.

## Building

```bash
browser/scripts/checkout-platform.sh macos build/uc-macos
browser/scripts/apply-cohelm.sh build/uc-macos flags.macos.gn
# then the platform repo's own build, from build/uc-macos
```

On Windows the platform is `windows` and the flags file is `flags.windows.gn`. `checkout-platform.sh` refuses a tag that doesn't carry the pinned ungoogled-chromium release.

## Packaging

After the build, make the Mac app and disk image, or the Windows installer, from the browser build and a release build of the Helper:

```bash
browser/packaging/package-macos.sh path/to/Chromium.app helper/target/release/cohelm-helper 0.1.0 dist
```

```powershell
browser/packaging/package-windows.ps1 -Browser path\to\build -Helper helper\target\release\cohelm-helper.exe -Version 0.1.0 -Out dist
```

The Windows script needs Inno Setup 6. Both download Node and Chrome DevTools MCP at the versions pinned in [`packaging/runtime.env`](packaging/runtime.env) and stop if a checksum doesn't match. To check an installed Cohelm, point `COHELM_PACKAGED_HELPER` at its Helper and run `cargo test --release --test packaged` in `helper/`.

## Moving to a new release

ungoogled-chromium usually follows a Chromium security release within days. Cohelm aims to follow within a week.

1. Find the new release and the matching tags in [ungoogled-chromium-macos](https://github.com/ungoogled-software/ungoogled-chromium-macos/tags) and [ungoogled-chromium-windows](https://github.com/ungoogled-software/ungoogled-chromium-windows/tags). If one platform has no tag yet, wait or ship the other first.
2. Update all three lines in `ungoogled-chromium.env` in one commit.
3. For each patch in `patches/series`, check it still applies to the new source: unpack the source with the platform repo, apply upstream's patches, then `patch -p1 --dry-run` each Cohelm patch in order. Refresh any that fail, and drop any the new release made unneeded.
4. Check each flag in `flags.gn` still exists: `gn args out/Default --list --short` lists them. A flag that no longer exists is removed, and the reason goes in the commit.
5. Build both platforms on the build machines, run the release smoke test, and go through the privacy checklist below.

## Privacy checklist

Run on each full build before release. The goal is no traffic Cohelm didn't cause: no telemetry, no crash reports, no Google services.

- [ ] Start Cohelm with a fresh profile and `--log-net-log=netlog.json --net-log-capture-mode=Default`.
- [ ] Leave it idle on the new-tab page for 5 minutes, then quit.
- [ ] Load `netlog.json` in [the NetLog viewer](https://netlog-viewer.appspot.com/) and list every host contacted.
- [ ] Confirm the list is empty, apart from the Helper's update check to `api.github.com`. Anything else blocks the release.
- [ ] Confirm `chrome://crashes` says crash reporting is off.

## Licenses

ungoogled-chromium is BSD-3-Clause, and Chromium is BSD-3-Clause with many third-party licenses. Each build ships Chromium's own `about:credits` page, and the root [`NOTICE`](../NOTICE) names both projects.
