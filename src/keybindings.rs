use std::collections::HashMap;
use std::process;

use winit::keyboard::{KeyCode, ModifiersState};

use crate::TOYTERM_CONFIG;

const MOD_CTRL: u8 = 1 << 0;
const MOD_SHIFT: u8 = 1 << 1;
const MOD_ALT: u8 = 1 << 2;
const MOD_SUPER: u8 = 1 << 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutAction {
    NewTab,
    OpenLauncher,
    ClosePane,
    NextTab,
    PrevTab,
    SplitVertical,
    SplitHorizontal,
    ToggleSidebar,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    ResizeUp,
    ResizeDown,
    ResizeLeft,
    ResizeRight,
    IncreaseFont,
    DecreaseFont,
    Copy,
    Paste,
    ClearHistory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct KeyBinding {
    modifiers: u8,
    key: KeyCode,
}

impl KeyBinding {
    fn from_event(modifiers: ModifiersState, key: KeyCode) -> Self {
        let mut mask = 0;
        if modifiers.control_key() {
            mask |= MOD_CTRL;
        }
        if modifiers.shift_key() {
            mask |= MOD_SHIFT;
        }
        if modifiers.alt_key() {
            mask |= MOD_ALT;
        }
        if modifiers.super_key() {
            mask |= MOD_SUPER;
        }
        Self {
            modifiers: mask,
            key,
        }
    }

    fn display(self) -> String {
        let mut parts = Vec::new();
        if self.modifiers & MOD_CTRL != 0 {
            parts.push("Ctrl".to_owned());
        }
        if self.modifiers & MOD_SHIFT != 0 {
            parts.push("Shift".to_owned());
        }
        if self.modifiers & MOD_ALT != 0 {
            parts.push("Alt".to_owned());
        }
        if self.modifiers & MOD_SUPER != 0 {
            parts.push("Super".to_owned());
        }
        parts.push(key_name(self.key).to_owned());
        parts.join("+")
    }
}

lazy_static::lazy_static! {
    pub(crate) static ref KEYBINDINGS: HashMap<KeyBinding, ShortcutAction> =
        build_or_exit();
}

pub(crate) fn lookup(modifiers: ModifiersState, key: KeyCode) -> Option<ShortcutAction> {
    KEYBINDINGS
        .get(&KeyBinding::from_event(modifiers, key))
        .copied()
}

fn build_or_exit() -> HashMap<KeyBinding, ShortcutAction> {
    match build() {
        Ok(bindings) => bindings,
        Err(message) => {
            eprintln!("{}", message);
            process::exit(1);
        }
    }
}

fn build() -> Result<HashMap<KeyBinding, ShortcutAction>, String> {
    let mut action_bindings = Vec::new();
    for &(name, default, action) in default_bindings() {
        let key = match TOYTERM_CONFIG.keybindings.get(name) {
            Some(value) => parse_keybinding(name, value)?,
            None => parse_keybinding(name, default)?,
        };
        action_bindings.push((name, key, action));
    }

    for name in TOYTERM_CONFIG.keybindings.keys() {
        if !default_bindings().iter().any(|(known, _, _)| known == name) {
            return Err(format!("keybindings.{} は不明なアクション名です", name));
        }
    }

    let mut map = HashMap::new();
    let mut seen: HashMap<KeyBinding, &'static str> = HashMap::new();
    for (name, key, action) in action_bindings {
        if let Some(first) = seen.insert(key, name) {
            return Err(format!(
                "キーバインドが重複しています: \"{}\" が {} と {} の両方に割り当てられています",
                key.display(),
                first,
                name
            ));
        }
        map.insert(key, action);
    }
    Ok(map)
}

fn parse_keybinding(action_name: &str, value: &str) -> Result<KeyBinding, String> {
    let tokens: Vec<&str> = value.split('+').map(str::trim).collect();
    if tokens.is_empty() || tokens.iter().any(|token| token.is_empty()) {
        return Err(invalid(action_name, value, "空のキー指定です"));
    }

    let (modifier_tokens, key_tokens) = tokens.split_at(tokens.len().saturating_sub(1));
    let key_token = key_tokens
        .first()
        .ok_or_else(|| invalid(action_name, value, "キー本体がありません"))?;

    let mut modifiers = 0;
    for token in modifier_tokens {
        let bit = match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => MOD_CTRL,
            "shift" => MOD_SHIFT,
            "alt" => MOD_ALT,
            "super" => MOD_SUPER,
            _ => {
                return Err(invalid(
                    action_name,
                    value,
                    &format!("不明な修飾キー名 \"{}\"", token),
                ));
            }
        };
        if modifiers & bit != 0 {
            return Err(invalid(
                action_name,
                value,
                &format!("修飾キー \"{}\" が重複しています", token),
            ));
        }
        modifiers |= bit;
    }

    if modifiers == 0 {
        return Err(invalid(
            action_name,
            value,
            "修飾キー（Ctrl/Shift/Alt/Super）が最低1つ必要です",
        ));
    }

    let key = parse_key_name(key_token).ok_or_else(|| {
        invalid(
            action_name,
            value,
            &format!("不明なキー名 \"{}\"", key_token),
        )
    })?;
    Ok(KeyBinding { modifiers, key })
}

fn invalid(action_name: &str, value: &str, reason: &str) -> String {
    format!(
        "keybindings.{} = \"{}\" は不正です: {}",
        action_name, value, reason
    )
}

fn parse_key_name(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "a" => Some(KeyCode::KeyA),
        "b" => Some(KeyCode::KeyB),
        "c" => Some(KeyCode::KeyC),
        "d" => Some(KeyCode::KeyD),
        "e" => Some(KeyCode::KeyE),
        "f" => Some(KeyCode::KeyF),
        "g" => Some(KeyCode::KeyG),
        "h" => Some(KeyCode::KeyH),
        "i" => Some(KeyCode::KeyI),
        "j" => Some(KeyCode::KeyJ),
        "k" => Some(KeyCode::KeyK),
        "l" => Some(KeyCode::KeyL),
        "m" => Some(KeyCode::KeyM),
        "n" => Some(KeyCode::KeyN),
        "o" => Some(KeyCode::KeyO),
        "p" => Some(KeyCode::KeyP),
        "q" => Some(KeyCode::KeyQ),
        "r" => Some(KeyCode::KeyR),
        "s" => Some(KeyCode::KeyS),
        "t" => Some(KeyCode::KeyT),
        "u" => Some(KeyCode::KeyU),
        "v" => Some(KeyCode::KeyV),
        "w" => Some(KeyCode::KeyW),
        "x" => Some(KeyCode::KeyX),
        "y" => Some(KeyCode::KeyY),
        "z" => Some(KeyCode::KeyZ),
        "0" => Some(KeyCode::Digit0),
        "1" => Some(KeyCode::Digit1),
        "2" => Some(KeyCode::Digit2),
        "3" => Some(KeyCode::Digit3),
        "4" => Some(KeyCode::Digit4),
        "5" => Some(KeyCode::Digit5),
        "6" => Some(KeyCode::Digit6),
        "7" => Some(KeyCode::Digit7),
        "8" => Some(KeyCode::Digit8),
        "9" => Some(KeyCode::Digit9),
        "f1" => Some(KeyCode::F1),
        "f2" => Some(KeyCode::F2),
        "f3" => Some(KeyCode::F3),
        "f4" => Some(KeyCode::F4),
        "f5" => Some(KeyCode::F5),
        "f6" => Some(KeyCode::F6),
        "f7" => Some(KeyCode::F7),
        "f8" => Some(KeyCode::F8),
        "f9" => Some(KeyCode::F9),
        "f10" => Some(KeyCode::F10),
        "f11" => Some(KeyCode::F11),
        "f12" => Some(KeyCode::F12),
        "tab" => Some(KeyCode::Tab),
        "delete" => Some(KeyCode::Delete),
        "backspace" => Some(KeyCode::Backspace),
        "enter" => Some(KeyCode::Enter),
        "escape" | "esc" => Some(KeyCode::Escape),
        "space" => Some(KeyCode::Space),
        "up" => Some(KeyCode::ArrowUp),
        "down" => Some(KeyCode::ArrowDown),
        "left" => Some(KeyCode::ArrowLeft),
        "right" => Some(KeyCode::ArrowRight),
        "minus" | "-" => Some(KeyCode::Minus),
        "equal" | "=" => Some(KeyCode::Equal),
        _ => None,
    }
}

fn key_name(key: KeyCode) -> &'static str {
    match key {
        KeyCode::KeyA => "A",
        KeyCode::KeyB => "B",
        KeyCode::KeyC => "C",
        KeyCode::KeyD => "D",
        KeyCode::KeyE => "E",
        KeyCode::KeyF => "F",
        KeyCode::KeyG => "G",
        KeyCode::KeyH => "H",
        KeyCode::KeyI => "I",
        KeyCode::KeyJ => "J",
        KeyCode::KeyK => "K",
        KeyCode::KeyL => "L",
        KeyCode::KeyM => "M",
        KeyCode::KeyN => "N",
        KeyCode::KeyO => "O",
        KeyCode::KeyP => "P",
        KeyCode::KeyQ => "Q",
        KeyCode::KeyR => "R",
        KeyCode::KeyS => "S",
        KeyCode::KeyT => "T",
        KeyCode::KeyU => "U",
        KeyCode::KeyV => "V",
        KeyCode::KeyW => "W",
        KeyCode::KeyX => "X",
        KeyCode::KeyY => "Y",
        KeyCode::KeyZ => "Z",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::F1 => "F1",
        KeyCode::F2 => "F2",
        KeyCode::F3 => "F3",
        KeyCode::F4 => "F4",
        KeyCode::F5 => "F5",
        KeyCode::F6 => "F6",
        KeyCode::F7 => "F7",
        KeyCode::F8 => "F8",
        KeyCode::F9 => "F9",
        KeyCode::F10 => "F10",
        KeyCode::F11 => "F11",
        KeyCode::F12 => "F12",
        KeyCode::Tab => "Tab",
        KeyCode::Delete => "Delete",
        KeyCode::Backspace => "Backspace",
        KeyCode::Enter => "Enter",
        KeyCode::Escape => "Escape",
        KeyCode::Space => "Space",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::Minus => "Minus",
        KeyCode::Equal => "Equal",
        _ => "Unknown",
    }
}

fn default_bindings() -> &'static [(&'static str, &'static str, ShortcutAction)] {
    &[
        ("new_tab", "Ctrl+Shift+T", ShortcutAction::NewTab),
        (
            "open_launcher",
            "Ctrl+Shift+N",
            ShortcutAction::OpenLauncher,
        ),
        ("close_pane", "Ctrl+Shift+W", ShortcutAction::ClosePane),
        ("next_tab", "Ctrl+Tab", ShortcutAction::NextTab),
        ("prev_tab", "Ctrl+Shift+Tab", ShortcutAction::PrevTab),
        (
            "split_vertical",
            "Ctrl+Shift+E",
            ShortcutAction::SplitVertical,
        ),
        (
            "split_horizontal",
            "Ctrl+Shift+O",
            ShortcutAction::SplitHorizontal,
        ),
        (
            "toggle_sidebar",
            "Ctrl+Shift+F",
            ShortcutAction::ToggleSidebar,
        ),
        ("focus_left", "Ctrl+Shift+H", ShortcutAction::FocusLeft),
        ("focus_down", "Ctrl+Shift+J", ShortcutAction::FocusDown),
        ("focus_up", "Ctrl+Shift+K", ShortcutAction::FocusUp),
        ("focus_right", "Ctrl+Shift+L", ShortcutAction::FocusRight),
        ("resize_up", "Ctrl+Shift+Up", ShortcutAction::ResizeUp),
        ("resize_down", "Ctrl+Shift+Down", ShortcutAction::ResizeDown),
        ("resize_left", "Ctrl+Shift+Left", ShortcutAction::ResizeLeft),
        (
            "resize_right",
            "Ctrl+Shift+Right",
            ShortcutAction::ResizeRight,
        ),
        ("increase_font", "Ctrl+Equal", ShortcutAction::IncreaseFont),
        ("decrease_font", "Ctrl+Minus", ShortcutAction::DecreaseFont),
        ("copy", "Ctrl+Shift+C", ShortcutAction::Copy),
        ("paste", "Ctrl+Shift+V", ShortcutAction::Paste),
        (
            "clear_history",
            "Ctrl+Shift+Delete",
            ShortcutAction::ClearHistory,
        ),
    ]
}
