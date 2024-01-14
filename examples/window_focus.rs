use dioxus::prelude::*;
use dioxus_desktop::tao::event::Event as WryEvent;
use dioxus_desktop::tao::event::WindowEvent;
use dioxus_desktop::use_wry_event_handler;
use dioxus_desktop::{Config, WindowCloseBehaviour};

fn main() {
    let cfg = Config::new().with_close_behaviour(WindowCloseBehaviour::CloseWindow);

    dioxus_desktop::launch_cfg(app, cfg);
}

fn app() -> Element {
    let focused = use_signal(|| false);

    use_wry_event_handler(move |event, _| {
        if let WryEvent::WindowEvent {
            event: WindowEvent::Focused(new_focused),
            ..
        } = event
        {
            focused.set(*new_focused);
        }
    });

    rsx! {
        div{
            width: "100%",
            height: "100%",
            display: "flex",
            flex_direction: "column",
            align_items: "center",
            {
                if *focused.get() {
                    "This window is focused!"
                } else {
                    "This window is not focused!"
                }
            }
        }
    }
}
