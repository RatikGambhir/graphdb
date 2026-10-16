//! Ratik Atlas: a desktop graph explorer for RatikDB, built with GPUI.
//!
//! Query with RQL, browse the declared schema, draw the graph, inspect any
//! node or edge, and write: inserts, updates and deletes run as explicit,
//! atomic transactions.

mod app;
mod assets;
mod client;
mod graph;
mod results;
mod rql;
mod schema;
mod settings;
mod theme;
mod ui;

use gpui::{
    App, Application, Bounds, Menu, MenuItem, SystemMenuType, TitlebarOptions, WindowBounds,
    WindowOptions, prelude::*, px, size,
};

use crate::app::{Atlas, OpenConnection, Quit, ShowGraph, ShowQuery, ShowSchema, ToggleTheme};

fn main() {
    Application::new()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            theme::install(cx, app::initial_mode());
            cx.bind_keys(ui::text_field::key_bindings());
            cx.bind_keys(app::key_bindings());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus(vec![
                Menu {
                    name: "Ratik Atlas".into(),
                    items: vec![
                        MenuItem::action("Connection…", OpenConnection),
                        MenuItem::separator(),
                        MenuItem::os_submenu("Services", SystemMenuType::Services),
                        MenuItem::separator(),
                        MenuItem::action("Quit Ratik Atlas", Quit),
                    ],
                },
                Menu {
                    name: "View".into(),
                    items: vec![
                        MenuItem::action("Query", ShowQuery),
                        MenuItem::action("Schema", ShowSchema),
                        MenuItem::action("Graph", ShowGraph),
                        MenuItem::separator(),
                        MenuItem::action("Toggle Theme", ToggleTheme),
                    ],
                },
            ]);
            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Ratik Atlas".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(960.), px(640.))),
                    app_id: Some("com.ratikdb.atlas".into()),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Atlas::new(window, cx)),
            );
            if let Err(error) = window {
                eprintln!("ratik-atlas: could not open a window: {error}");
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx| cx.quit()).detach();
            cx.activate(true);
        });
}
