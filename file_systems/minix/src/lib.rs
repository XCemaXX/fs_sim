use fs_api::prelude::*;

pub fn registrations() -> Vec<FsEntry> {
    vec![FsEntry {
        reg: Registration {
            id: "minix".into(),
            display_name: "MINIX".into(),
            family: "minix".into(),
            params: ParamSchema { fields: vec![] },
        },
        driver: None,
    }]
}
