//! `SeaORM` entity for retained opaque file content metadata.

use sea_orm::entity::prelude::*;

#[derive(Copy, Clone, Default, Debug, DeriveEntity)]
pub struct Entity;

impl EntityName for Entity {
    fn schema_name(&self) -> Option<&str> {
        Some("public")
    }
    fn table_name(&self) -> &str {
        "w9pt_fs_state_content_metadata"
    }
}

#[derive(Clone, Debug, PartialEq, DeriveModel, DeriveActiveModel, Eq)]
pub struct Model {
    pub filesystem_id: Vec<u8>,
    pub content_file_id: Vec<u8>,
    pub owner_inode_id: Vec<u8>,
    pub context_id: Vec<u8>,
    pub policy_format: i32,
    pub policy_bytes: Vec<u8>,
    pub key_commitment: Option<Vec<u8>>,
    pub wrapped_key_bytes: Option<Vec<u8>>,
    pub record_revision: Decimal,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveColumn)]
pub enum Column {
    FilesystemId,
    ContentFileId,
    OwnerInodeId,
    ContextId,
    PolicyFormat,
    PolicyBytes,
    KeyCommitment,
    WrappedKeyBytes,
    RecordRevision,
}

impl ColumnTrait for Column {
    type EntityName = Entity;
    fn def(&self) -> ColumnDef {
        match self {
            Self::FilesystemId
            | Self::ContentFileId
            | Self::OwnerInodeId
            | Self::ContextId
            | Self::PolicyBytes => ColumnType::VarBinary(StringLen::None).def(),
            Self::KeyCommitment | Self::WrappedKeyBytes => {
                ColumnType::VarBinary(StringLen::None).def().null()
            }
            Self::PolicyFormat => ColumnType::Integer.def(),
            Self::RecordRevision => ColumnType::Decimal(Some((20, 0))).def(),
        }
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DerivePrimaryKey)]
pub enum PrimaryKey {
    FilesystemId,
    ContentFileId,
}

impl PrimaryKeyTrait for PrimaryKey {
    type ValueType = (Vec<u8>, Vec<u8>);
    fn auto_increment() -> bool {
        false
    }
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match *self {}
    }
}

impl ActiveModelBehavior for ActiveModel {}
