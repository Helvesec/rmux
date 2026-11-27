use crate::screen::{Screen, TITLE_STACK_MAX};

impl Screen {
    pub(super) fn accept_window_name(&mut self, name: &str) {
        // `ESC k` is a window rename, not a title: tmux gates it on
        // `allow-rename` (input.c:2812) and leaves `allow-set-title` to OSC
        // 0/2 and APC. The accepted request is recorded before the stored
        // metadata is compared, because a repeated or empty name still renames
        // the window and still notifies.
        if !self.allow_rename {
            return;
        }
        self.record_window_rename_request(name);
        if self.window_name != name {
            self.window_name = name.to_owned();
            self.bump_metadata_revision();
        }
    }

    pub(super) fn update_path(&mut self, path: &str) {
        if self.path != path {
            self.path = path.to_owned();
            self.bump_metadata_revision();
        }
    }

    pub(super) fn push_title_stack(&mut self) {
        if !self.title_rename_enabled {
            return;
        }
        if self.title_stack.len() >= TITLE_STACK_MAX {
            let excess = self.title_stack.len() + 1 - TITLE_STACK_MAX;
            self.title_stack.drain(0..excess);
        }
        self.title_stack.push(self.title.clone());
        self.bump_metadata_revision();
    }

    pub(super) fn pop_title_stack(&mut self) {
        if !self.title_rename_enabled {
            return;
        }
        if let Some(title) = self.title_stack.pop() {
            self.title = title;
            self.bump_metadata_revision();
        }
    }
}
