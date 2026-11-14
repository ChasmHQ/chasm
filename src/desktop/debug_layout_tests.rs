use super::*;
use serde_json::json;

#[test]
fn debugger_panes_keep_session_and_fields_at_minimum_and_normal_viewports() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = MainWindow::new().unwrap();
    ui.show().unwrap();
    let root = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut initial = DesktopState::new(root.path().into(), 1.0);
    initial.activity = 4;
    initial.layout = layout::Layout::preset(layout::Preset::Debugging);
    let start = initial.debug.begin_start().unwrap();
    let envelope = json!({"id":"layout-fixture","generation":1,"revision":1,"sequence":1,"state":"paused","busy":false,"snapshot":{"status":{"stopped":true,"pc":32,"location":{"file":"Fixture.sol","line":2}},"capabilities":{"source":true,"locals":true,"solidity":true,"restart":false},"panes":{"source":{"file":"Fixture.sol","items":[{"line":1,"text":"contract Fixture {","current":false},{"line":2,"text":"uint256 value;","current":true}]},"disassembly":{"rows":[{"pc":32,"text":"PUSH1 0x20","current":true}]},"frames":{"items":[{"name":"Fixture.run()","depth":0}]},"locals":{"items":[{"name":"value","value":"25"}]},"memory":{"offset":0,"items":[{"offset":0,"data":"0x00000000000000000000000000000019"}]},"stack":{"items":["0x19"]}}},"error":null,"result":{"evaluation":{"display":"25","kept":false}}});
    initial.debug.accept(&start, &envelope).unwrap();
    let state = Arc::new(Mutex::new(initial));
    wire(&ui, &state, runtime.handle().clone());
    let investigation = Arc::new(investigation::Investigation::new(root.path().join("cases")));
    native_investigation::wire(&ui, &state, &investigation, runtime.handle().clone());
    panels::bind(&ui, &state, runtime.handle().clone(), investigation);
    ui.global::<DebugLogic>().invoke_draft_edited("evaluate-input".into(), "feeBps".into());
    for (width, height) in [(1180.0, 720.0), (1360.0, 820.0)] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        for (index, zoom) in session::ZOOM_LEVELS.iter().enumerate() {
            {
                let mut state = state.lock().unwrap();
                state.zoom = *zoom;
                state.zoom_index = index as i32;
                state.layout = layout::Layout::preset(layout::Preset::Debugging);
                state.layout.hide(&layout::PaneId::new(layout::PaneKind::Navigation, None)).unwrap();
                state.layout.bottom_height = 170.0;
            }
            for view in 0..5 {
                {
                    let mut state = state.lock().unwrap();
                    let kind = if view == 0 { layout::PaneKind::DebugInspector }
                        else if view <= 2 { layout::PaneKind::DebugInspection }
                        else { layout::PaneKind::DebugSession };
                    state.layout.select(layout::Zone::Right, &layout::PaneId::new(kind, None)).unwrap();
                    state.debug.select_inspection(if view == 2 { 2 } else { 1 });
                }
                panels::refresh_all();
                assert_eq!(ui.global::<PanelData>().get_center_kind(), 4);
                assert_eq!(ui.global::<PanelData>().get_right_kind(), if view == 0 { 5 } else if view <= 2 { 6 } else { 7 });
                let pixels = ui.window().take_snapshot().unwrap();
                assert_eq!(pixels.width(), width as u32);
                assert_eq!(pixels.height(), height as u32);
                if let Some(directory) = std::env::var_os("CHASM_DEBUG_LAYOUT_CAPTURES") {
                    use std::io::Write;
                    let directory = std::path::PathBuf::from(directory);
                    assert!(directory.is_absolute() && directory.is_dir(), "Capture directory must already exist and be absolute");
                    let target = directory.join(format!("debug-{width:.0}x{height:.0}-{}-view{view}.ppm", (zoom * 100.0).round() as u32));
                    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(target).unwrap();
                    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height()).unwrap();
                    for pixel in pixels.as_slice() { file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap(); }
                }
            }
            ui.global::<DebugLogic>().invoke_inspector_tab(1);
            ui.global::<DebugLogic>().invoke_inspect_tab(2);
            ui.global::<Logic>().invoke_open_activity(2);
            ui.global::<Logic>().invoke_open_activity(4);
            let state = state.lock().unwrap();
            assert_eq!(state.debug.envelope.as_ref().unwrap().id, "layout-fixture");
            assert_eq!(state.debug.inspector_tab, 1);
            assert_eq!(state.debug.inspect_tab, 2);
            assert_eq!(ui.global::<DebugData>().get_evaluate_input(), "feeBps");
            assert!(ui.global::<DebugData>().get_result_text().contains("25"));
        }
    }
    panels::shutdown();
    ui.hide().unwrap();
}
