#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod builder;
mod chunk;
mod color_tables;
mod colors;
mod leveldb;
mod live;
mod live_game;
mod live_tiles;
mod nbt;
mod panel;
mod pngio;
mod protocol;
mod render;
mod viewer;
mod world;
mod worlds;

fn main() {
    tauri::Builder::default()
        .manage(panel::AppState::new())
        .setup(|app| {
            use tauri::Manager;
            // The live link builds maps and opens map windows by itself (when the in-game pack reports a world).
            app.state::<panel::AppState>().live.set_app(app.handle().clone());
            Ok(())
        })
        .register_uri_scheme_protocol("maps", |ctx, request| {
            use tauri::Manager;
            let state = ctx.app_handle().state::<panel::AppState>();
            protocol::respond(&state.maps_dir, &state.live, request.uri().path())
        })
        .invoke_handler(tauri::generate_handler![
            panel::list_worlds,
            panel::add_world,
            panel::start_build,
            panel::job_status,
            panel::open_map,
            live::live_status,
            live::live_configure,
            live::live_new_key,
            live::live_link,
            live::live_disconnect,
            live::live_pack,
            live::live_set_auto_open,
            live::live_enable_content_log
        ])
        .run(tauri::generate_context!())
        .expect("error while running Bedrock Map");
}
