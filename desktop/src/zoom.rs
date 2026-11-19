//! Board zoom below the native titlebar. Rune's scale transforms drawing and input, not layout;
//! lay out the board in an inverse-sized viewport so every surface still fits/reflows.
use super::App;
use rune::{Cx, El, Key, KeyEvent, Tx, el};

#[derive(Default)]
pub(super) struct Zoom {
    // Integer steps avoid drift and make reset / reciprocal shortcuts exact.
    step: i8,
}

impl Zoom {
    pub(super) fn maximum() -> Self { Self { step: 8 } }
    pub(super) fn factor(&self) -> f32 { 1.1_f32.powi(i32::from(self.step)) }

    pub(super) fn load(&mut self) {
        let Some(path) = super::preferences_path().map(|p| p.with_file_name("zoom.json")) else { return; };
        if let Some(step) = std::fs::read(path).ok().and_then(|data| serde_json::from_slice::<i8>(&data).ok()) {
            self.step = step.clamp(-6, 8);
        }
    }

    pub(super) fn save(&self) -> std::io::Result<()> {
        let path = super::preferences_path().ok_or_else(|| std::io::Error::other("No settings directory available"))?.with_file_name("zoom.json");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.step.to_string())?;
        std::fs::rename(tmp, path)
    }

    pub(super) fn key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        // `cmd` is Rune's primary modifier (Ctrl elsewhere, Command on macOS).
        // Also accept explicit Control; never steal AltGr or plain punctuation.
        if !(event.mods.cmd || event.mods.ctrl) || event.mods.alt { return false; }
        let value = match &event.key {
            Key::Text(s) => s.as_str(),
            Key::Command(c) => match c { '+' => "+", '=' => "=", '-' => "-", '0' => "0", _ => return false },
            _ => return false,
        };
        self.step = match value {
            "+" | "=" => (self.step + 1).min(8),
            "-" if !event.mods.shift => (self.step - 1).max(-6),
            "0" if !event.mods.shift => 0,
            _ => return false,
        };
        tx.instant();
        true
    }
}

pub(super) fn viewport(app: &App, cx: &mut Cx<App>) -> [f32; 2] {
    let [width, height] = cx.layout_size("zoom-viewport").unwrap_or([1252.0, 674.0]);
    let scale = app.zoom.factor();
    [width / scale, height / scale]
}

pub(super) fn wrap(app: &App, cx: &mut Cx<App>, content: El) -> El {
    let [width, height] = viewport(app, cx);
    el().key("zoom-viewport").w_full().h(0.0).grow(1.0).clip_soft(0.0)
        .child(content.absolute(0.0, 0.0).size(width, height)
            .origin(0.0, 0.0).scale(app.zoom.factor()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Mods, Runtime};

    fn settle(rt: &mut Runtime<App>) { for _ in 0..90 { rt.frame(1.0 / 60.0); } }
    fn primary() -> Mods { Mods { cmd: true, ..Default::default() } }
    fn press(rt: &mut Runtime<App>, key: Key, mods: Mods) {
        rt.key(KeyEvent { key, mods });
        settle(rt);
    }
    fn runtime(size: (f32, f32), dpi: f32) -> Runtime<App> {
        let mut app = App::new();
        crate::fixture::load(&mut app, true);
        let mut rt = Runtime::new(app, crate::view::root, size, Env::new(true, dpi));
        rt.on_key = Some(|a, e, tx| a.zoom.key(e, tx));
        settle(&mut rt);
        rt
    }

    #[test]
    fn overlays_fit_at_max_zoom_and_calendar_scrolls_without_moving_actions() {
        let mut rt = runtime((620., 420.), 2.);
        let mut insets = rune::ChromeInsets::default();
        insets.leading = 76.; insets.height = 30.;
        rt.set_chrome_insets(insets);
        rt.update(|a, _| a.zoom = Zoom::maximum());
        let fits = |rt: &Runtime<App>, key: &str| {
            let [x, y, w, h] = element(rt, key).rect;
            assert!(x >= 0. && y >= 29. && x + w <= 621. && y + h <= 421., "{key}: {x}, {y}, {w}, {h}");
        };
        for page in [crate::Dialog::Settings, crate::Dialog::Personal, crate::Dialog::BoardView,
            crate::Dialog::Connection, crate::Dialog::Share, crate::Dialog::Tags, crate::Dialog::ReplyTags(1),
            crate::Dialog::NewChannel, crate::Dialog::DeleteThread(1), crate::Dialog::Schedule] {
            rt.update(|a, _| { a.navigate(crate::Page::Tasks); a.dialog = Some(page); });
            settle(&mut rt);
            fits(&rt, "board-dialog");
            fits(&rt, "dialog-close");
        }
        fits(&rt, "calendar-confirm");
        let confirm = element(&rt, "calendar-confirm").rect;
        let [x,y,w,h] = element(&rt, "modal-body").rect;
        let pending_before = element(&rt, "calendar-pending").rect[1];
        rt.wheel(x+w/2.,y+h/2.,-900.);
        settle(&mut rt);
        assert!(element(&rt, "calendar-pending").rect[1] < pending_before);
        assert_eq!(element(&rt, "calendar-confirm").rect, confirm);
        rt.update(|a, _| { a.dialog = None; a.setup_open = true; });
        settle(&mut rt);
        fits(&rt, "setup-drawer");
        rt.update(|a, _| { a.setup_open = false; a.begin_add_board(); a.onboarding.joining = true; });
        settle(&mut rt);
        fits(&rt, "board-onboarding");
    }
    fn element(rt: &Runtime<App>, key: &str) -> rune::ElementInfo {
        rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap()
    }
    fn close(a: f32, b: f32) { assert!((a-b).abs() < 1.0, "{a} != {b}"); }

    #[test]
    fn native_titlebar_reserves_space_above_the_board_at_any_zoom() {
        let mut rt = runtime((800.0, 600.0), 2.0);
        for (leading, trailing) in [(76.0, 0.0), (0.0, 138.0), (0.0, 0.0)] {
            let mut insets = rune::ChromeInsets::default();
            insets.leading = leading;
            insets.trailing = trailing;
            let height = if leading + trailing > 0.0 { crate::view::TITLEBAR_H } else { 0.0 };
            insets.height = height;
            rt.set_chrome_insets(insets);
            press(&mut rt, Key::Command('='), primary());
            let [_, y, _, h] = element(&rt, "sidebar").rect;
            close(y, height);
            close(y + h, 600.0);
            assert_eq!(rt.find("titlebar").is_some(), height > 0.0);
        }
    }

    #[test]
    fn zoom_aliases_limits_and_unrelated_keys() {
        let mut zoom = Zoom::default();
        let mut tx = Tx::default();
        for key in [Key::Text("=".into()), Key::Text("+".into()), Key::Command('+')] {
            assert!(zoom.key(&KeyEvent { key, mods: Mods { shift: true, ..primary() } }, &mut tx));
        }
        assert_eq!(zoom.step, 3);
        for s in ["=", "+", "-", "0", ",", "a"] {
            assert!(!zoom.key(&KeyEvent { key: Key::Text(s.into()), mods: Mods::default() }, &mut tx));
        }
        assert!(!zoom.key(&KeyEvent { key: Key::Command('='), mods: Mods { alt: true, ..primary() } }, &mut tx));
        for _ in 0..100 { zoom.key(&KeyEvent { key: Key::Command('='), mods: primary() }, &mut tx); }
        assert_eq!(zoom.step, 8);
        for _ in 0..100 { zoom.key(&KeyEvent { key: Key::Command('-'), mods: Mods { ctrl: true, ..Default::default() } }, &mut tx); }
        assert_eq!(zoom.step, -6);
        assert!(zoom.key(&KeyEvent { key: Key::Command('0'), mods: primary() }, &mut tx));
        assert_eq!(zoom.factor(), 1.0);
    }

    #[test]
    fn zoom_scales_native_chrome_and_reflows_to_window_at_each_dpi() {
        for dpi in [1.0, 2.0] {
            let mut rt = runtime((1252.0, 674.0), dpi);
            let before = element(&rt, "sidebar").rect;
            press(&mut rt, Key::Command('='), primary());
            let after = element(&rt, "sidebar");
            close(after.rect[2], before[2] * 1.1);
            close(after.rect[3], 674.0);
            close(element(&rt, "header").rect[3], 45.0 * 1.1);
            let root = element(&rt, "neochat").rect;
            close(root[0], 0.0); close(root[1], 0.0);
            close(root[2], 1252.0); close(root[3], 674.0);
            close(rt.state.width, 1252.0 / 1.1);
            press(&mut rt, Key::Command('0'), primary());
            close(element(&rt, "sidebar").rect[2], before[2]);
        }
    }

    #[test]
    fn zoomed_hit_testing_and_thread_breakpoint_use_scaled_layout() {
        let mut rt = runtime((1000.0, 674.0), 1.0);
        rt.update(|a, _| a.open_thread(1)); settle(&mut rt);
        assert!(rt.find("sidebar").is_some());
        press(&mut rt, Key::Command('='), primary());
        press(&mut rt, Key::Command('='), primary());
        assert!(rt.state.width < 900.0);
        assert!(rt.find("sidebar").is_none());
        let input = element(&rt, "reply-input");
        let [x,y,w,h] = input.visible;
        assert!(w > 0.0 && h > 0.0);
        rt.pointer_down(x+w/2.0, y+h/2.0);
        rt.pointer_up(x+w/2.0, y+h/2.0);
        settle(&mut rt);
        press(&mut rt, Key::Text("zoomed reply".into()), Mods::default());
        assert_eq!(rt.state.reply.text, "zoomed reply");
        assert!(rt.state.draft.text.is_empty());
        press(&mut rt, Key::Command('0'), primary());
        assert!(rt.find("sidebar").is_some());
        assert_eq!(rt.state.reply.text, "zoomed reply");
    }

    #[test]
    fn focused_composer_zoom_keeps_text_selection_undo_and_plain_punctuation() {
        let mut rt = runtime((700.0, 600.0), 1.0);
        rt.update(|a, tx| { a.navigate(crate::Page::Channel("neoboard/dev".into())); tx.focus("post-input", false); }); settle(&mut rt);
        press(&mut rt, Key::Text("draft".into()), Mods::default());
        press(&mut rt, Key::Text("+".into()), Mods { shift: true, ..primary() });
        assert_eq!(rt.state.draft.text, "draft");
        assert_eq!(rt.state.zoom.step, 1);
        assert!(element(&rt, "post-input").focused);
        press(&mut rt, Key::Command('a'), primary());
        press(&mut rt, Key::Text("replacement".into()), Mods::default());
        assert_eq!(rt.state.draft.text, "replacement");
        press(&mut rt, Key::Command('z'), primary());
        assert_eq!(rt.state.draft.text, "draft");
        press(&mut rt, Key::Command('0'), primary());
        press(&mut rt, Key::Command('a'), primary());
        press(&mut rt, Key::Text("=+-0,".into()), Mods::default());
        assert_eq!(rt.state.draft.text, "=+-0,");
        assert_eq!(rt.state.zoom.step, 0);
        close(element(&rt, "neochat").rect[2], 700.0);
    }
}