//! Renders every main view at the supported window sizes and zoom levels.
//! Set `CHASM_VIEW_CAPTURES` to an existing absolute directory to write PPM
//! captures for visual review; without it the test only checks that every
//! view renders at the expected size. `CHASM_VIEW_ONLY` narrows the views.
use super::*;
use serde_json::json;

fn bank_state(root: &std::path::Path) -> DesktopState {
    let raw = json!({"type":"compile_success","contracts":[{"name":"Bank","artifact":{
        "bytecode":{"object":"6000"},
        "abi":[
            {"type":"constructor","inputs":[{"name":"limit","type":"uint256"}],"stateMutability":"nonpayable"},
            {"type":"function","name":"balanceOf","inputs":[{"name":"owner","type":"address"}],"outputs":[{"name":"","type":"uint256"}],"stateMutability":"view"},
            {"type":"function","name":"transfer","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[{"name":"","type":"bool"}],"stateMutability":"nonpayable"},
            {"type":"event","name":"Transfer","inputs":[{"name":"from","type":"address","indexed":true},{"name":"to","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false}],"anonymous":false}
        ]
    }}]});
    let mut state = DesktopState::new(root.into(), 1.0);
    state.contracts = session::contracts_from_compile(&raw.to_string()).unwrap();
    state.compile_label = "1 contracts".into();
    // A paused session so the debugger views show the execution card and code.
    let start = state.debug.begin_start().unwrap();
    let envelope = json!({"id":"capture-fixture","generation":1,"revision":1,"sequence":1,"state":"paused","busy":false,"snapshot":{
        "status":{"stopped":true,"pc":50,"mnemonic":"SSTORE","depth":1,"step":812,"gas":{"remaining":29000000},"location":{"file":"WorkshopVault.sol","line":20}},
        "capabilities":{"source":true,"locals":true,"solidity":true,"restart":true},
        "panes":{"source":{"file":"WorkshopVault.sol","items":[{"line":19,"text":"    function withdraw(uint256 amount) external {","current":false},{"line":20,"text":"        balances[msg.sender] -= amount;","current":true}]},
                 "frames":{"items":[{"name":"WorkshopVault.withdraw(uint256)","depth":0}]},"locals":{"items":[{"name":"amount","value":"25"}]},"stack":{"items":["0x19"]}}},
        "error":null,"result":null});
    state.debug.accept(&start, &envelope).unwrap();
    state
}

#[test]
fn every_view_renders_at_supported_sizes_and_zooms() {
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
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let state = Arc::new(Mutex::new(bank_state(root.path())));
    wire(&ui, &state, runtime.handle().clone());
    let investigation = Arc::new(investigation::Investigation::new(root.path().join("cases")));
    native_investigation::wire(&ui, &state, &investigation, runtime.handle().clone());
    panels::bind(&ui, &state, runtime.handle().clone(), investigation);
    let directory = std::env::var_os("CHASM_VIEW_CAPTURES").map(std::path::PathBuf::from);
    if let Some(directory) = &directory {
        assert!(directory.is_absolute() && directory.is_dir(), "Capture directory must already exist and be absolute");
    }
    // `CHASM_VIEW_ONLY=debugger,wallet` limits a capture run to those views.
    let only = std::env::var("CHASM_VIEW_ONLY").ok();
    // (name, preparation) applied to a fresh layout for each capture.
    let views: [(&str, fn(&mut DesktopState)); 9] = [
        ("contract", |state| state.select_contract(0)),
        ("deploy", |state| { state.select_contract(0); state.open_deploy_doc(); }),
        ("wallet", |state| state.open_wallet_doc()),
        ("explorer", |state| state.open_activity(1)),
        ("converter", |state| state.open_activity(2)),
        ("macros", |state| state.open_activity(3)),
        ("foundry", |state| state.open_foundry_build()),
        ("debugger", |state| state.open_activity(4)),
        ("investigation", |state| state.open_activity(0)),
    ];
    for (width, height) in [(1180.0, 720.0), (1360.0, 820.0), (1600.0, 960.0)] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        for (index, zoom) in session::ZOOM_LEVELS.iter().enumerate() {
            for (name, prepare) in views {
                if only.as_ref().is_some_and(|only| !only.split(',').any(|view| view == name)) {
                    continue;
                }
                {
                    let mut state = state.lock().unwrap();
                    state.zoom = *zoom;
                    state.zoom_index = index as i32;
                    state.layout = layout::Layout::preset(if name == "debugger" {
                        layout::Preset::Debugging
                    } else if name == "investigation" {
                        layout::Preset::Investigation
                    } else {
                        layout::Preset::Contracts
                    });
                    prepare(&mut state);
                }
                panels::focus_current();
                panels::refresh_all();
                let pixels = ui.window().take_snapshot().unwrap();
                assert_eq!((pixels.width(), pixels.height()), (width as u32, height as u32));
                if let Some(directory) = &directory {
                    use std::io::Write;
                    let target = directory.join(format!("{name}-{width:.0}x{height:.0}-{}.ppm", (zoom * 100.0).round() as u32));
                    let mut file = std::fs::File::create(target).unwrap();
                    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height()).unwrap();
                    for pixel in pixels.as_slice() {
                        file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
                    }
                }
            }
        }
    }
    panels::shutdown();
    ui.hide().unwrap();
}
