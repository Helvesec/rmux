use rmux_core::{BoxLines, Style};
use rmux_proto::Target;

use super::mouse::{mouse_drag, mouse_release};
use crate::input_keys::MouseForwardEvent;
use crate::pane_terminals::HandlerState;
use crate::renderer::{render_menu_overlay, MenuRenderItem, MenuRenderSpec, OverlayRect};

use super::super::prompt_support::PromptInputEvent;
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequesterOrigin;
use super::identity::OverlayIdentity;

pub(super) const MENU_STAYOPEN: u8 = 0x01;
pub(super) const MENU_NOMOUSE: u8 = 0x02;
pub(super) const MENU_TAB: u8 = 0x04;

#[derive(Debug, Clone)]
pub(in crate::handler) struct MenuOverlayState {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) identity: OverlayIdentity,
    pub(in crate::handler) origin: RequesterOrigin,
    pub(in crate::handler) command_context: QueueExecutionContext,
    pub(in crate::handler) current_target: Target,
    pub(in crate::handler) rect: OverlayRect,
    pub(in crate::handler) title: String,
    pub(in crate::handler) style: Style,
    pub(in crate::handler) selected_style: Style,
    pub(in crate::handler) border_style: Style,
    pub(in crate::handler) border_lines: BoxLines,
    pub(in crate::handler) flags: u8,
    pub(in crate::handler) choice: Option<usize>,
    pub(in crate::handler) items: Vec<MenuOverlayItem>,
}

impl MenuOverlayState {
    pub(super) fn render(&self) -> Vec<u8> {
        render_menu_overlay(&MenuRenderSpec {
            rect: self.rect,
            title: self.title.clone(),
            style: self.style.clone(),
            selected_style: self.selected_style.clone(),
            border_style: self.border_style.clone(),
            border_lines: self.border_lines,
            items: self
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| MenuRenderItem {
                    label: item.label.clone(),
                    shortcut: item.shortcut_label.clone(),
                    separator: item.separator,
                    selected: self.choice == Some(index),
                })
                .collect(),
        })
    }

    fn selectable_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (!item.separator).then_some(index))
    }

    fn first_selectable(&self) -> Option<usize> {
        self.selectable_indices().next()
    }

    fn last_selectable(&self) -> Option<usize> {
        self.selectable_indices().last()
    }

    fn advance(&self, current: Option<usize>, forward: bool) -> Option<usize> {
        let selectable = self.selectable_indices().collect::<Vec<_>>();
        if selectable.is_empty() {
            return None;
        }
        let current_pos = current
            .and_then(|choice| selectable.iter().position(|index| *index == choice))
            .unwrap_or(0);
        let next_pos = if forward {
            (current_pos + 1) % selectable.len()
        } else {
            (current_pos + selectable.len() - 1) % selectable.len()
        };
        selectable.get(next_pos).copied()
    }

    fn page_move(&self, current: Option<usize>, forward: bool) -> Option<usize> {
        let selectable = self.selectable_indices().collect::<Vec<_>>();
        if selectable.is_empty() {
            return None;
        }
        let current_pos = current
            .and_then(|choice| selectable.iter().position(|index| *index == choice))
            .unwrap_or(0);
        let delta = 5;
        let next_pos = if forward {
            (current_pos + delta).min(selectable.len().saturating_sub(1))
        } else {
            current_pos.saturating_sub(delta)
        };
        selectable.get(next_pos).copied()
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct MenuOverlayItem {
    pub(in crate::handler) label: String,
    pub(in crate::handler) shortcut_label: Option<String>,
    pub(in crate::handler) shortcut: Option<PromptInputEvent>,
    pub(in crate::handler) separator: bool,
    pub(in crate::handler) action: Option<OverlayMenuAction>,
}

#[derive(Debug, Clone)]
pub(in crate::handler) enum OverlayMenuAction {
    Command(String),
    Popup(PopupMenuAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum PopupMenuAction {
    Close,
    Paste,
    FillSpace,
    Centre,
    HorizontalPane,
    VerticalPane,
}

#[derive(Debug, Clone)]
pub(super) enum MenuOutcome {
    Stay,
    Redraw,
    Close,
    Execute(OverlayMenuAction),
}

pub(super) fn menu_handle_event(
    menu: &mut MenuOverlayState,
    event: PromptInputEvent,
) -> MenuOutcome {
    if let Some(action) = menu
        .items
        .iter()
        .find(|item| !item.separator && item.shortcut.as_ref() == Some(&event))
        .and_then(|item| item.action.clone())
    {
        return MenuOutcome::Execute(action);
    }

    match event {
        PromptInputEvent::Up | PromptInputEvent::Ctrl('p') | PromptInputEvent::Char('k') => {
            menu.choice = menu
                .advance(menu.choice, false)
                .or_else(|| menu.last_selectable());
            MenuOutcome::Redraw
        }
        PromptInputEvent::Down | PromptInputEvent::Ctrl('n') | PromptInputEvent::Char('j') => {
            menu.choice = menu
                .advance(menu.choice, true)
                .or_else(|| menu.first_selectable());
            MenuOutcome::Redraw
        }
        PromptInputEvent::Tab if (menu.flags & MENU_TAB) != 0 => {
            menu.choice = menu
                .advance(menu.choice, true)
                .or_else(|| menu.first_selectable());
            MenuOutcome::Redraw
        }
        PromptInputEvent::KeyName(name) if name == "PageUp" => {
            menu.choice = menu.page_move(menu.choice, false);
            MenuOutcome::Redraw
        }
        PromptInputEvent::KeyName(name) if name == "PageDown" => {
            menu.choice = menu.page_move(menu.choice, true);
            MenuOutcome::Redraw
        }
        PromptInputEvent::Home | PromptInputEvent::Char('g') => {
            menu.choice = menu.first_selectable();
            MenuOutcome::Redraw
        }
        PromptInputEvent::End | PromptInputEvent::Char('G') => {
            menu.choice = menu.last_selectable();
            MenuOutcome::Redraw
        }
        PromptInputEvent::Enter => menu
            .choice
            .and_then(|choice| menu.items.get(choice))
            .and_then(|item| item.action.clone())
            .map_or(MenuOutcome::Close, MenuOutcome::Execute),
        PromptInputEvent::Escape
        | PromptInputEvent::Ctrl('c')
        | PromptInputEvent::Ctrl('g')
        | PromptInputEvent::Char('q') => MenuOutcome::Close,
        _ => MenuOutcome::Stay,
    }
}

pub(super) fn menu_handle_mouse(
    menu: &mut MenuOverlayState,
    raw: MouseForwardEvent,
) -> MenuOutcome {
    let within = raw.x >= menu.rect.x
        && raw.x < menu.rect.x.saturating_add(menu.rect.width)
        && raw.y >= menu.rect.y
        && raw.y < menu.rect.y.saturating_add(menu.rect.height);
    if (menu.flags & MENU_NOMOUSE) != 0 {
        return MenuOutcome::Stay;
    }
    if !within {
        if mouse_drag(raw.b) {
            return MenuOutcome::Stay;
        }
        if (menu.flags & MENU_STAYOPEN) != 0 && menu.choice.take().is_some() {
            return MenuOutcome::Redraw;
        }
        return MenuOutcome::Close;
    }

    if raw.y <= menu.rect.y
        || raw.y
            >= menu
                .rect
                .y
                .saturating_add(menu.rect.height.saturating_sub(1))
    {
        return MenuOutcome::Stay;
    }

    let item_row = raw.y.saturating_sub(menu.rect.y.saturating_add(1));
    let choice = usize::from(item_row);
    if choice >= menu.items.len() {
        return MenuOutcome::Stay;
    }
    if menu.items.get(choice).is_some_and(|item| item.separator) {
        if menu.choice.take().is_some() {
            return MenuOutcome::Redraw;
        }
        return MenuOutcome::Stay;
    }
    let choice_changed = menu.choice != Some(choice);
    menu.choice = Some(choice);
    let choose_now = if (menu.flags & MENU_STAYOPEN) != 0 {
        !mouse_release(raw.b) && !mouse_drag(raw.b)
    } else {
        mouse_release(raw.b)
    };
    if choose_now {
        return menu
            .items
            .get(choice)
            .and_then(|item| item.action.clone())
            .map_or(MenuOutcome::Close, MenuOutcome::Execute);
    }
    if choice_changed {
        MenuOutcome::Redraw
    } else {
        MenuOutcome::Stay
    }
}

pub(super) fn popup_menu_items(state: &HandlerState) -> Vec<MenuOverlayItem> {
    let mut items = vec![MenuOverlayItem {
        label: "Close".to_owned(),
        shortcut_label: Some("q".to_owned()),
        shortcut: Some(PromptInputEvent::Char('q')),
        separator: false,
        action: Some(OverlayMenuAction::Popup(PopupMenuAction::Close)),
    }];

    if state.buffers.stack_head().is_some() {
        items.push(MenuOverlayItem {
            label: "Paste".to_owned(),
            shortcut_label: Some("p".to_owned()),
            shortcut: Some(PromptInputEvent::Char('p')),
            separator: false,
            action: Some(OverlayMenuAction::Popup(PopupMenuAction::Paste)),
        });
    }

    items.push(MenuOverlayItem {
        label: String::new(),
        shortcut_label: None,
        shortcut: None,
        separator: true,
        action: None,
    });
    items.push(MenuOverlayItem {
        label: "Fill Space".to_owned(),
        shortcut_label: Some("F".to_owned()),
        shortcut: Some(PromptInputEvent::Char('F')),
        separator: false,
        action: Some(OverlayMenuAction::Popup(PopupMenuAction::FillSpace)),
    });
    items.push(MenuOverlayItem {
        label: "Centre".to_owned(),
        shortcut_label: Some("C".to_owned()),
        shortcut: Some(PromptInputEvent::Char('C')),
        separator: false,
        action: Some(OverlayMenuAction::Popup(PopupMenuAction::Centre)),
    });
    items.push(MenuOverlayItem {
        label: String::new(),
        shortcut_label: None,
        shortcut: None,
        separator: true,
        action: None,
    });
    items.push(MenuOverlayItem {
        label: "To Horizontal Pane".to_owned(),
        shortcut_label: Some("h".to_owned()),
        shortcut: Some(PromptInputEvent::Char('h')),
        separator: false,
        action: Some(OverlayMenuAction::Popup(PopupMenuAction::HorizontalPane)),
    });
    items.push(MenuOverlayItem {
        label: "To Vertical Pane".to_owned(),
        shortcut_label: Some("v".to_owned()),
        shortcut: Some(PromptInputEvent::Char('v')),
        separator: false,
        action: Some(OverlayMenuAction::Popup(PopupMenuAction::VerticalPane)),
    });
    items
}
