//! keylight-gui — LimeLight desktop GUI (Slint).
//! Talks to keylightd HTTP API; see docs/API.md.

use i_slint_backend_winit::{EventResult, WinitWindowAccessor};
use std::cell::RefCell;
use std::rc::Rc;
use winit::event::WindowEvent;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let drag_in_progress = Rc::new(RefCell::new(false));
    let cursor_left_during_drag = Rc::new(RefCell::new(false));

    ui.on_request_quit(|| {
        let _ = slint::quit_event_loop();
    });

    ui.on_request_minimize({
        let ui_weak = ui.as_weak();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    w.set_minimized(true);
                });
            }
        }
    });

    ui.on_request_add(|| {
        // TODO: wire up add-light / add-group logic
    });

    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();

    ui.window().on_winit_window_event({
        let drag_in_progress = Rc::clone(&drag_in_progress);
        let cursor_left_during_drag = Rc::clone(&cursor_left_during_drag);
        let ui_weak = ui.as_weak();
        move |_w, event| {
            match event {
                WindowEvent::MouseInput { .. } => {}
                WindowEvent::Touch { .. } => {}
                WindowEvent::CursorMoved { .. } => {
                    let in_drag = *drag_in_progress.borrow();
                    if in_drag {
                        *drag_in_progress.borrow_mut() = false;
                        *cursor_left_during_drag.borrow_mut() = false;
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.invoke_reset_drag_state();
                        }
                    } else {
                        // CursorMoved fires constantly when not in drag; skip logging to avoid flood
                    }
                }
                WindowEvent::CursorEntered { .. } => {
                    let in_drag = *drag_in_progress.borrow();
                    let had_left = *cursor_left_during_drag.borrow();
                    if in_drag && had_left {
                        *drag_in_progress.borrow_mut() = false;
                        *cursor_left_during_drag.borrow_mut() = false;
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.invoke_reset_drag_state();
                        }
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    if *drag_in_progress.borrow() {
                        *cursor_left_during_drag.borrow_mut() = true;
                    }
                }
                _ => {}
            }
            EventResult::Propagate
        }
    });

    ui.on_start_window_drag({
        let drag_in_progress = Rc::clone(&drag_in_progress);
        let ui_weak = ui.as_weak();
        let is_wayland = is_wayland;
        move || {
            if !is_wayland {
                return;
            }
            *drag_in_progress.borrow_mut() = true;
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                    w.set_cursor(winit::window::CursorIcon::Default);
                });
            }
        }
    });

    ui.on_drag_window_by({
        let ui_weak = ui.as_weak();
        move |dx, dy| {
            if is_wayland {
                return;
            }
            if dx == 0 && dy == 0 {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    if let Ok(pos) = w.outer_position() {
                        let new_pos = winit::dpi::PhysicalPosition::new(pos.x + dx, pos.y + dy);
                        w.set_outer_position(new_pos);
                    }
                });
            }
        }
    });

    ui.run()
}
