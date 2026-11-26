# Build on ungoogled-chromium, not from scratch

Cohelm starts from an existing browser and fills the gaps with open-source parts, so most of the work is packaging, patching and stabilising rather than writing a browser. We picked ungoogled-chromium (BSD-3-Clause) as the base: it is a full Chromium browser, so sites and Chrome extensions behave as users expect, and its license fits Cohelm's Apache 2.0.

## Considered Options

- **Helium and BrowserOS**: rejected. Their own code is GPL-3.0 and AGPL-3.0, which would force Cohelm off Apache 2.0.
- **A Firefox fork (Floorp or Zen, MPL-2.0)**: faster builds and license-compatible per file, but not Chromium.
- **CEF (BSD)**: quick builds, but only an engine, so we would write the whole browser UI ourselves.

## Consequences

Full Chromium builds take hours and need a large build machine, so release builds can't run on small CI runners.
