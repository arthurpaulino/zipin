//! Rime-backed input engine. Concrete struct, no trait.
//!
//! Owns the librime API handle plus a session id. Each mutation feeds into
//! Rime via `process_key` / `select_*` / `change_page`, then `refresh`
//! pulls a fresh `Context` snapshot so the UI can render without touching
//! the FFI layer.

use anyhow::{anyhow, Context as _, Result};
use include_dir::{include_dir, Dir};
use rime_sys::{Api, RimeSessionId, KEY_BACKSPACE};
use std::fs;
use std::path::{Path, PathBuf};

pub use rime_sys::Candidate;

const ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../assets/rime");
/// Bumped whenever the bundled assets change. Triggers a re-unpack into the
/// shared data dir.
const ASSET_VERSION: &str = "v0.1.0";

pub struct Page<'a> {
    pub candidates: &'a [Candidate],
    pub has_prev: bool,
    pub has_next: bool,
    pub highlighted: usize,
}

pub struct Engine {
    api: Api,
    session: RimeSessionId,
    candidates: Vec<Candidate>,
    page_no: usize,
    is_last_page: bool,
    highlighted: usize,
    composing: bool,
    pending_commit: String,
    ascii_mode: bool,
}

impl Engine {
    pub fn new() -> Result<Self> {
        let dirs = data_dirs()?;
        migrate_legacy_layout(&dirs)?;
        let unpacked = unpack_assets(&dirs.shared)?;

        let api = Api::get().ok_or_else(|| anyhow!("rime_get_api_stdbool returned null"))?;
        let mut traits = rime_sys::Traits::new(&dirs.shared, &dirs.user);
        api.setup(&mut traits);
        api.initialize(&mut traits);

        // `start_maintenance(true)` returns true when it actually has work
        // to do (cold first launch, schema bump, asset re-unpack). Warm
        // launches return false and `join_maintenance_thread` is a no-op.
        // Only surface the user-facing message when we are about to wait.
        let deploying = api.start_maintenance(true);
        if unpacked || deploying {
            eprintln!("zipin: deploying Rime dictionaries (a few seconds on first launch)...");
        }
        api.join_maintenance_thread();

        let session = api.create_session();
        if session == 0 {
            return Err(anyhow!("rime create_session returned 0"));
        }

        // Default to simplified output via OpenCC (`zh_simp` option group).
        api.set_option(session, "zh_simp", true);

        let mut engine = Self {
            api,
            session,
            candidates: Vec::new(),
            page_no: 0,
            is_last_page: true,
            highlighted: 0,
            composing: false,
            pending_commit: String::new(),
            ascii_mode: false,
        };
        engine.refresh();
        Ok(engine)
    }

    pub fn is_composing(&self) -> bool {
        self.composing
    }

    pub fn ascii_mode(&self) -> bool {
        self.ascii_mode
    }

    pub fn toggle_ascii(&mut self) {
        self.ascii_mode = !self.ascii_mode;
        self.api
            .set_option(self.session, "ascii_mode", self.ascii_mode);
        // Drop any in-flight composition so the new mode takes effect on
        // the next keystroke instead of mid-word.
        self.reset();
    }

    pub fn feed(&mut self, c: char) {
        self.api.process_key(self.session, c as i32, 0);
        self.refresh();
    }

    pub fn shrink(&mut self) {
        if !self.composing {
            return;
        }
        self.api.process_key(self.session, KEY_BACKSPACE, 0);
        self.refresh();
    }

    pub fn reset(&mut self) {
        self.api.clear_composition(self.session);
        self.clear_state();
        self.pending_commit.clear();
    }

    fn clear_state(&mut self) {
        self.candidates.clear();
        self.page_no = 0;
        self.is_last_page = true;
        self.highlighted = 0;
        self.composing = false;
    }

    pub fn current_page(&self) -> Option<Page<'_>> {
        if self.candidates.is_empty() {
            return None;
        }
        Some(Page {
            candidates: &self.candidates,
            has_prev: self.page_no > 0,
            has_next: !self.is_last_page,
            highlighted: self.highlighted,
        })
    }

    pub fn next_candidate(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        if self.highlighted + 1 < self.candidates.len() {
            self.api
                .highlight_candidate_on_current_page(self.session, self.highlighted + 1);
            self.refresh();
        } else if !self.is_last_page {
            self.api.change_page(self.session, false);
            self.refresh();
        }
    }

    pub fn prev_candidate(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        if self.highlighted > 0 {
            self.api
                .highlight_candidate_on_current_page(self.session, self.highlighted - 1);
            self.refresh();
        } else if self.page_no > 0 {
            self.api.change_page(self.session, true);
            self.refresh();
            // Highlight the last candidate on the now-current page.
            if !self.candidates.is_empty() {
                let last = self.candidates.len() - 1;
                self.api
                    .highlight_candidate_on_current_page(self.session, last);
                self.refresh();
            }
        }
    }

    pub fn next_page(&mut self) {
        if !self.is_last_page {
            self.api.change_page(self.session, false);
            self.refresh();
        }
    }

    pub fn prev_page(&mut self) {
        if self.page_no > 0 {
            self.api.change_page(self.session, true);
            self.refresh();
        }
    }

    pub fn commit_highlighted(&mut self) -> Option<String> {
        if self.candidates.is_empty() {
            return None;
        }
        self.api
            .select_candidate_on_current_page(self.session, self.highlighted);
        self.refresh();
        self.take_commit()
    }

    pub fn commit_index_on_page(&mut self, idx: usize) -> Option<String> {
        if idx >= self.candidates.len() {
            return None;
        }
        self.api.select_candidate_on_current_page(self.session, idx);
        self.refresh();
        self.take_commit()
    }

    pub fn take_commit(&mut self) -> Option<String> {
        if self.pending_commit.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending_commit))
        }
    }

    fn refresh(&mut self) {
        if let Some(text) = self.api.get_commit(self.session) {
            self.pending_commit.push_str(&text);
        }
        match self.api.get_context(self.session) {
            Some(ctx) => {
                self.composing = ctx.composing;
                self.candidates = ctx.candidates;
                self.page_no = ctx.page_no;
                self.is_last_page = ctx.is_last_page;
                self.highlighted = ctx.highlighted;
            }
            None => self.clear_state(),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.api.destroy_session(self.session);
        self.api.finalize();
    }
}

/// Two-dir split: `shared/` is fully owned by zipin (overwritten on every
/// asset upgrade); `user/` is fully owned by the user (custom YAMLs, learned
/// userdbs, deployed `.bin` files). Rime's `shared_data_dir` and
/// `user_data_dir` traits get them respectively, so user customizations
/// survive `ASSET_VERSION` bumps.
pub struct Dirs {
    pub root: PathBuf,
    pub shared: PathBuf,
    pub user: PathBuf,
}

pub fn data_dirs() -> Result<Dirs> {
    let root = dirs::data_dir()
        .ok_or_else(|| anyhow!("could not resolve XDG data dir"))?
        .join("zipin")
        .join("rime");
    let shared = root.join("shared");
    let user = root.join("user");
    fs::create_dir_all(&shared).with_context(|| format!("create {}", shared.display()))?;
    fs::create_dir_all(&user).with_context(|| format!("create {}", user.display()))?;
    Ok(Dirs { root, shared, user })
}

/// One-time migration from the legacy single-dir layout (everything in
/// `rime/`) to the split layout (`rime/shared/` + `rime/user/`). Detected
/// by the asset-version stamp sitting at the legacy root. User-owned
/// content (`*.userdb/`, `*.custom.yaml`, `user.yaml`, `build/`) is moved
/// into `user/`; bundled defaults are deleted so the next `unpack_assets`
/// call rewrites them under `shared/`.
fn migrate_legacy_layout(dirs: &Dirs) -> Result<()> {
    let legacy_stamp = dirs.root.join(".zipin-asset-version");
    if !legacy_stamp.exists() {
        return Ok(());
    }
    eprintln!("zipin: migrating legacy data layout into shared/ + user/...");
    for entry in fs::read_dir(&dirs.root)? {
        let entry = entry?;
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n == "shared" || n == "user" {
            continue;
        }
        let src = entry.path();
        let is_user_owned = n.ends_with(".userdb")
            || n.ends_with(".userdb.txt")
            || n.ends_with(".custom.yaml")
            || n == "user.yaml"
            || n == "build";
        if is_user_owned {
            let dst = dirs.user.join(&name);
            fs::rename(&src, &dst).ok();
        } else if src.is_file() {
            fs::remove_file(&src).ok();
        } else if src.is_dir() {
            fs::remove_dir_all(&src).ok();
        }
    }
    Ok(())
}

fn unpack_assets(target: &Path) -> Result<bool> {
    let stamp = target.join(".zipin-asset-version");
    if let Ok(existing) = fs::read_to_string(&stamp) {
        if existing.trim() == ASSET_VERSION {
            return Ok(false);
        }
    }
    write_dir(&ASSETS, target)?;
    fs::write(&stamp, ASSET_VERSION).context("write asset version stamp")?;
    Ok(true)
}

fn write_dir(dir: &Dir<'_>, base: &Path) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::File(f) => {
                let dst = base.join(f.path());
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dst, f.contents())
                    .with_context(|| format!("write {}", dst.display()))?;
            }
            include_dir::DirEntry::Dir(d) => {
                let sub = base.join(d.path());
                fs::create_dir_all(&sub)?;
                write_dir(d, base)?;
            }
        }
    }
    Ok(())
}
