//! Where an installed Cohelm keeps its parts, next to the Helper.
//!
//! The Helper is the program the person starts. The browser, the built-in
//! extension, Chrome DevTools MCP and the Node runtime it needs all sit beside
//! it (ADR 0006):
//!
//! ```text
//! Mac                                   Windows
//! Cohelm.app/Contents/                  Cohelm\
//!   MacOS/Cohelm            (Helper)      Cohelm.exe          (Helper)
//!   Resources/                            browser\chrome.exe
//!     Cohelm Browser.app                  extension\
//!     extension/                          devtools-mcp\
//!     devtools-mcp/                       node\node.exe
//!     node/bin/node
//! ```

use std::path::{Path, PathBuf};

use crate::DevtoolsCommand;

/// The entry point inside the Chrome DevTools MCP package.
const DEVTOOLS_MCP_SCRIPT: &str = "out/Default/gen/src/bin/chrome-devtools-mcp.js";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    /// Not shipped in 0.1; laid out like Windows for local builds.
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub browser: PathBuf,
    pub extension: PathBuf,
    pub node: PathBuf,
    pub devtools_mcp: PathBuf,
}

impl Layout {
    /// The parts that belong beside the Helper at `exe`.
    pub fn beside(exe: &Path, platform: Platform) -> Option<Self> {
        let dir = exe.parent()?;
        Some(match platform {
            Platform::Mac => {
                let resources = dir.parent()?.join("Resources");
                Self {
                    browser: mac_browser(&resources.join("Cohelm Browser.app/Contents/MacOS")),
                    extension: resources.join("extension"),
                    node: resources.join("node/bin/node"),
                    devtools_mcp: resources.join("devtools-mcp").join(DEVTOOLS_MCP_SCRIPT),
                }
            }
            Platform::Windows => Self {
                browser: dir.join("browser").join("chrome.exe"),
                extension: dir.join("extension"),
                node: dir.join("node").join("node.exe"),
                devtools_mcp: dir.join("devtools-mcp").join(DEVTOOLS_MCP_SCRIPT),
            },
            Platform::Other => Self {
                browser: dir.join("browser").join("chrome"),
                extension: dir.join("extension"),
                node: dir.join("node").join("bin").join("node"),
                devtools_mcp: dir.join("devtools-mcp").join(DEVTOOLS_MCP_SCRIPT),
            },
        })
    }

    /// The installed Cohelm's parts, if the running Helper is installed.
    pub fn installed() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        Self::beside(&exe, Platform::current()).filter(|l| l.browser.is_file())
    }

    /// The bundled Chrome DevTools MCP, run with the bundled Node.
    pub fn devtools(&self) -> DevtoolsCommand {
        DevtoolsCommand {
            program: self.node.clone().into(),
            args: vec![self.devtools_mcp.clone().into()],
        }
    }
}

/// The browser is the one program in its app's `Contents/MacOS` folder:
/// `Chromium` in a Cohelm build, or another name in a stand-in such as Chrome
/// for Testing.
fn mac_browser(macos: &Path) -> PathBuf {
    let programs: Vec<PathBuf> = std::fs::read_dir(macos)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    match programs.as_slice() {
        [only] => only.clone(),
        _ => macos.join("Chromium"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_parts_sit_in_the_app_bundle() {
        let layout = Layout::beside(
            Path::new("/Applications/Cohelm.app/Contents/MacOS/Cohelm"),
            Platform::Mac,
        )
        .unwrap();
        let resources = Path::new("/Applications/Cohelm.app/Contents/Resources");
        assert_eq!(
            layout.browser,
            resources.join("Cohelm Browser.app/Contents/MacOS/Chromium")
        );
        assert_eq!(layout.extension, resources.join("extension"));
        assert_eq!(layout.node, resources.join("node/bin/node"));
        assert!(
            layout
                .devtools_mcp
                .starts_with(resources.join("devtools-mcp"))
        );
        assert!(layout.devtools_mcp.ends_with("chrome-devtools-mcp.js"));
    }

    #[test]
    fn the_mac_browser_is_the_one_program_in_its_app() {
        let app = tempfile::tempdir().unwrap();
        let contents = app.path().join("Cohelm.app/Contents");
        let macos = contents.join("Resources/Cohelm Browser.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("Google Chrome for Testing"), "").unwrap();
        let layout = Layout::beside(&contents.join("MacOS/Cohelm"), Platform::Mac).unwrap();
        assert_eq!(layout.browser, macos.join("Google Chrome for Testing"));
    }

    #[test]
    fn windows_parts_sit_beside_the_helper() {
        let dir = Path::new("C:/Users/ada/AppData/Local/Programs/Cohelm");
        let layout = Layout::beside(&dir.join("Cohelm.exe"), Platform::Windows).unwrap();
        assert_eq!(layout.browser, dir.join("browser").join("chrome.exe"));
        assert_eq!(layout.extension, dir.join("extension"));
        assert_eq!(layout.node, dir.join("node").join("node.exe"));
        let devtools = layout.devtools();
        assert_eq!(devtools.program, dir.join("node").join("node.exe"));
        assert_eq!(devtools.args, vec![layout.devtools_mcp.into_os_string()]);
    }

    #[test]
    fn a_helper_run_from_a_build_folder_is_not_installed() {
        // The test binary has no browser beside it.
        assert_eq!(Layout::installed(), None);
    }
}
