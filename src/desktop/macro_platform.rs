use super::native_macro::MacroPlatform;

pub struct NativeMacroPlatform;

impl MacroPlatform for NativeMacroPlatform {
    fn import_definition(&self) -> Result<Option<Vec<u8>>, ()> {
        super::platform::open_json_file(super::macro_model::MAX_MACRO_JSON_BYTES).map_err(|_| ())
    }

    fn export_definition(&self, json: &str) -> Result<bool, ()> {
        super::platform::save_json_file(json, "chasm-macro.json")
            .map(|path| path.is_some())
            .map_err(|_| ())
    }

    fn save_evidence(&self, json: &str) -> Result<bool, ()> {
        super::platform::save_json_file(json, "chasm-macro-evidence.json")
            .map(|path| path.is_some())
            .map_err(|_| ())
    }
}
