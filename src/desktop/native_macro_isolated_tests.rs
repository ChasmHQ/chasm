#![allow(dead_code, unused_imports)]

#[path = "."]
mod desktop {
    use slint::ModelRc;
    use std::path::PathBuf;

    slint::include_modules!();

    pub fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
        ModelRc::new(slint::VecModel::from(items))
    }

    pub struct DesktopState {
        pub workspace_ready: bool,
        pub workspace: PathBuf,
        pub workspace_epoch: u64,
        pub network_epoch: u64,
        pub active: bool,
        pub owned_node: Option<std::sync::Arc<owned_node::OwnedNode>>,
    }

    impl DesktopState {
        pub fn new(workspace: PathBuf, _: f32) -> Self {
            Self {
                workspace_ready: true,
                workspace,
                workspace_epoch: 1,
                network_epoch: 1,
                active: true,
                owned_node: None,
            }
        }

        pub fn transition_pending(&self) -> bool {
            !self.active
        }
        pub fn open_activity(&mut self, _: i32) {}
    }

    pub fn paint(_: &slint::Weak<MainWindow>, _: &std::sync::Arc<std::sync::Mutex<DesktopState>>) {}

    pub mod panels {
        pub fn focus_current() {}
    }

    pub mod owned_node {
        pub struct OwnedNode;
        impl OwnedNode {
            pub fn from_child_handle(_: usize, _: u16) -> std::sync::Arc<Self> {
                std::sync::Arc::new(Self)
            }
            pub fn valid(&self) -> bool {
                true
            }
        }
    }

    #[path = "macro_model.rs"]
    pub mod macro_model;
    #[path = "macro_runner.rs"]
    pub mod macro_runner;
    #[path = "native_macro.rs"]
    pub mod native_macro;
}
