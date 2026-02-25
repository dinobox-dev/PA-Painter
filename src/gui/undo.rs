use practical_arcana_painter::project::BaseColor;
use practical_arcana_painter::types::{Layer, OutputSettings, PresetLibrary};

/// Snapshot of undoable project state (excludes runtime caches).
#[derive(Debug, Clone, PartialEq)]
pub struct UndoSnapshot {
    pub layers: Vec<Layer>,
    pub settings: OutputSettings,
    pub base_color: BaseColor,
    pub base_normal: Option<String>,
    pub presets: PresetLibrary,
}

/// Number of stable (unchanged) frames before a batch of changes is committed.
const COALESCE_FRAMES: u32 = 1;

/// Undo/redo history with automatic change coalescing.
///
/// Coalescing strategy: continuous edits (e.g. slider drags that change state
/// every frame) are grouped into a single undo entry.  A snapshot is captured
/// when changes first begin, and committed to the undo stack only after the
/// state has been stable for `COALESCE_FRAMES` consecutive frames.
pub struct UndoHistory {
    undo_stack: Vec<UndoSnapshot>,
    redo_stack: Vec<UndoSnapshot>,
    max_depth: usize,

    /// Snapshot from before the current batch of changes started.
    pre_change: Option<UndoSnapshot>,

    /// How many consecutive frames the state has been stable after a change.
    stable_frames: u32,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_depth: 50,
            pre_change: None,
            stable_frames: 0,
        }
    }
}

impl UndoHistory {
    /// Call once per frame with the snapshot taken at the START of the frame,
    /// the current state at the END of the frame, and whether any pointer
    /// button is currently held down.
    ///
    /// Automatically detects changes and coalesces rapid edits.
    /// While `pointer_down` is true, pending changes are never committed —
    /// this prevents a single slider drag from splitting into multiple
    /// undo entries when the mouse pauses mid-drag.
    pub fn track_frame(
        &mut self,
        pre_frame: &UndoSnapshot,
        post_frame: &UndoSnapshot,
        pointer_down: bool,
    ) {
        let changed = pre_frame != post_frame;

        if changed {
            // State changed this frame
            if self.pre_change.is_none() {
                // First change in a new batch — save the "before" state
                self.pre_change = Some(pre_frame.clone());
            }
            self.stable_frames = 0;
        } else if self.pre_change.is_some() {
            if pointer_down {
                // Pointer held — drag may resume, don't commit yet
                self.stable_frames = 0;
            } else {
                // Pointer released and state stable
                self.stable_frames += 1;
                if self.stable_frames >= COALESCE_FRAMES {
                    self.commit_pending();
                }
            }
        }
    }

    /// Force-commit any pending change batch (e.g. before undo/redo or discrete action).
    pub fn flush(&mut self) {
        if self.pre_change.is_some() {
            self.commit_pending();
        }
    }

    /// Push an explicit undo snapshot (for discrete actions like add/delete layer).
    /// This also flushes any pending continuous edit.
    #[allow(dead_code)]
    pub fn push(&mut self, snapshot: UndoSnapshot) {
        self.commit_pending();
        self.undo_stack.push(snapshot);
        if self.undo_stack.len() > self.max_depth {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Undo: pop from undo stack, push current state to redo, return the restored snapshot.
    pub fn undo(&mut self, current: UndoSnapshot) -> Option<UndoSnapshot> {
        self.flush();
        let snapshot = self.undo_stack.pop()?;
        self.redo_stack.push(current);
        Some(snapshot)
    }

    /// Redo: pop from redo stack, push current state to undo, return the restored snapshot.
    pub fn redo(&mut self, current: UndoSnapshot) -> Option<UndoSnapshot> {
        let snapshot = self.redo_stack.pop()?;
        self.undo_stack.push(current);
        Some(snapshot)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Clear all history (e.g. on project load/new).
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.pre_change = None;
        self.stable_frames = 0;
    }

    fn commit_pending(&mut self) {
        if let Some(snapshot) = self.pre_change.take() {
            self.undo_stack.push(snapshot);
            if self.undo_stack.len() > self.max_depth {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
        self.stable_frames = 0;
    }
}
