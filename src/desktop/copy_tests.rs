use slint::{ComponentHandle, LogicalPosition, platform::{Clipboard, Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent}};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct ClipboardBackend {
    backend: i_slint_backend_testing::TestingBackend,
    clipboard: Rc<RefCell<String>>,
    writes: Rc<Cell<usize>>,
}

impl Platform for ClipboardBackend {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        self.backend.create_window_adapter()
    }

    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        if clipboard == Clipboard::DefaultClipboard {
            *self.clipboard.borrow_mut() = text.to_string();
            self.writes.set(self.writes.get() + 1);
        }
    }

    fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
        (clipboard == Clipboard::DefaultClipboard).then(|| self.clipboard.borrow().clone())
    }
}

slint::slint! {
    import { SelectableText } from "../../ui-slint/selectable-text.slint";
    import { Confirmation } from "../../ui-slint/main.slint";
    import { LineEdit, TextEdit } from "std-widgets.slint";
    export component CopyFixture inherits Window {
        width: 600px;
        height: 360px;
        in property <string> first: "Address fixture A";
        in property <string> second: "Path fixture B";
        in-out property <string> editable: "Editable fixture C";
        in-out property <bool> modal-visible: false;
        in-out property <int> cancelled-count: 0;
        in-out property <int> confirmed-count: 0;
        out property <string> first-text: first-view.text;
        out property <string> output-text: output.text;
        public function focus-first() { first-view.focus(); }
        public function focus-second() { second-view.focus(); }
        public function focus-editable() { input.focus(); }
        public function focus-output() { output.focus(); }
        public function select-substring() { first-view.set-selection-offsets(0, 7); }
        public function copy-substring() { first-view.copy(); }
        VerticalLayout {
            first-view := SelectableText { text: root.first; }
            second-view := SelectableText { text: root.second; }
            input := LineEdit { text <=> root.editable; }
            output := TextEdit { text: "Log one\nLog two"; read-only: true; }
        }
        if root.modal-visible: Confirmation {
            heading: "Delete fixture?";
            detail: "Only this file name";
            warning: "Keep a backup.";
            action-label: "Delete fixture";
            cancelled => { root.cancelled-count += 1; root.modal-visible = false; }
            confirmed => { root.confirmed-count += 1; root.modal-visible = false; }
        }
    }
}

fn shortcut(window: &slint::Window, key: &str) {
    window.dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Control.into() });
    window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Control.into() });
}

fn mouse_drag(window: &slint::Window, from: (f32, f32), to: (f32, f32)) {
    let (fx, fy) = from;
    let (tx, ty) = to;
    window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(fx, fy) });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: LogicalPosition::new(fx, fy),
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(tx, ty) });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(tx, ty),
        button: PointerEventButton::Left,
    });
}

fn right_click(window: &slint::Window, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Right,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Right,
    });
}

#[test]
fn focused_box_select_all_does_not_copy_and_copy_never_leaks_other_boxes() {
    let clipboard = Rc::new(RefCell::new("clipboard sentinel".into()));
    let writes = Rc::new(Cell::new(0));
    slint::platform::set_platform(Box::new(ClipboardBackend {
        backend: i_slint_backend_testing::TestingBackend::new(Default::default()),
        clipboard: clipboard.clone(), writes: writes.clone(),
    })).unwrap();
    let ui = CopyFixture::new().unwrap();
    ui.show().unwrap();
    ui.invoke_focus_first();
    shortcut(ui.window(), "a");
    assert_eq!(writes.get(), 0, "Ctrl+A must not write the clipboard");
    assert_eq!(clipboard.borrow().as_str(), "clipboard sentinel");
    shortcut(ui.window(), "c");
    assert_eq!(clipboard.borrow().as_str(), "Address fixture A");
    ui.invoke_focus_second();
    shortcut(ui.window(), "a");
    shortcut(ui.window(), "c");
    assert_eq!(clipboard.borrow().as_str(), "Path fixture B");
    ui.invoke_focus_first();
    ui.invoke_select_substring();
    ui.invoke_copy_substring();
    assert_eq!(clipboard.borrow().as_str(), "Address");
    shortcut(ui.window(), "a");
    shortcut(ui.window(), "x");
    shortcut(ui.window(), "v");
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: "z".into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: "z".into() });
    assert_eq!(ui.get_first_text(), "Address fixture A");
    ui.invoke_focus_output();
    shortcut(ui.window(), "a");
    shortcut(ui.window(), "c");
    assert_eq!(clipboard.borrow().as_str(), "Log one\nLog two");
    shortcut(ui.window(), "v");
    assert_eq!(ui.get_output_text(), "Log one\nLog two");
    ui.invoke_focus_editable();
    shortcut(ui.window(), "a");
    shortcut(ui.window(), "c");
    assert_eq!(clipboard.borrow().as_str(), "Editable fixture C");
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: "replacement".into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: "replacement".into() });
    assert_eq!(ui.get_editable(), "replacement");
    ui.set_modal_visible(true);
    let before_select_all = writes.get();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Tab.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Tab.into() });
    shortcut(ui.window(), "a");
    assert_eq!(writes.get(), before_select_all);
    shortcut(ui.window(), "c");
    assert_eq!(clipboard.borrow().as_str(), "Delete fixture?");
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Tab.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Tab.into() });
    shortcut(ui.window(), "a");
    shortcut(ui.window(), "c");
    assert_eq!(clipboard.borrow().as_str(), "Only this file name");
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
    assert_eq!(ui.get_cancelled_count(), 1);
    assert_eq!(ui.get_confirmed_count(), 0);
    assert!(!ui.get_modal_visible());
    mouse_drag(ui.window(), (1.0, 8.0), (590.0, 8.0));
    let before_drag_copy = writes.get();
    ui.invoke_copy_substring();
    assert_eq!(
        clipboard.borrow().as_str(),
        "Address fixture A",
        "mouse drag must select the whole line"
    );
    assert_eq!(
        writes.get(),
        before_drag_copy + 1,
        "drag selection itself must not write the clipboard"
    );
    ui.invoke_select_substring();
    ui.invoke_copy_substring();
    assert_eq!(clipboard.borrow().as_str(), "Address");
    let before_right_click = writes.get();
    right_click(ui.window(), 300.0, 8.0);
    assert_eq!(
        writes.get(),
        before_right_click,
        "right-click must open the context menu instead of writing the clipboard"
    );
    ui.invoke_copy_substring();
    assert_eq!(
        clipboard.borrow().as_str(),
        "Address",
        "right-click must leave the current selection intact"
    );
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
    assert_eq!(
        writes.get(),
        before_right_click + 1,
        "context menu dismissal must not write the clipboard"
    );
}
