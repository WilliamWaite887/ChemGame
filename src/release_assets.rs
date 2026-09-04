//! Asset reader used only by Steam release builds.

use std::{io, path::Path, sync::Arc};

use bevy::{
    asset::{
        io::{
            file::FileAssetReader, AssetReader, AssetReaderError, AssetSourceBuilder,
            AssetSourceId, PathStream, Reader, VecReader,
        },
        AssetApp,
    },
    prelude::App,
};

use crate::asset_pack_format::{decode_key, PackFile};

const PACKS: &[&str] = &["content_00.cgp", "content_01.cgp", "content_02.cgp"];

pub fn register(app: &mut App) {
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(|| {
            Box::new(ReleaseAssetReader::open().unwrap_or_else(|error| {
                panic!("Steam release assets could not be opened: {error}")
            }))
        }),
    );
}

struct ReleaseAssetReader {
    loose: FileAssetReader,
    packs: Vec<PackFile>,
}

impl ReleaseAssetReader {
    fn open() -> Result<Self, String> {
        let key = decode_key(
            option_env!("CHEMGAME_ASSET_KEY")
                .ok_or("this packed build was compiled without CHEMGAME_ASSET_KEY")?,
        )
        .map_err(|error| error.to_string())?;
        let root = FileAssetReader::get_base_path();
        let packs = PACKS
            .iter()
            .map(|name| PackFile::open(&root.join(name), key).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            loose: FileAssetReader::new("assets"),
            packs,
        })
    }

    fn is_public(path: &Path) -> bool {
        path.as_os_str().is_empty()
            || path
                .components()
                .next()
                .is_some_and(|part| part.as_os_str() == "sounds")
    }

    fn pack_for(&self, path: &Path) -> Option<&PackFile> {
        self.packs.iter().find(|pack| pack.contains(path))
    }

    fn io_error(error: impl ToString) -> AssetReaderError {
        AssetReaderError::Io(Arc::new(io::Error::new(
            io::ErrorKind::InvalidData,
            error.to_string(),
        )))
    }
}

impl AssetReader for ReleaseAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        if Self::is_public(path) {
            let reader = self.loose.read(path).await?;
            return Ok(Box::new(reader) as Box<dyn Reader>);
        }
        let pack = self
            .pack_for(path)
            .ok_or_else(|| AssetReaderError::NotFound(path.to_path_buf()))?;
        let bytes = pack.read(path).map_err(Self::io_error)?;
        Ok(Box::new(VecReader::new(bytes)) as Box<dyn Reader>)
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        if Self::is_public(path) {
            let reader = self.loose.read_meta(path).await?;
            return Ok(Box::new(reader) as Box<dyn Reader>);
        }
        Err(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        if Self::is_public(path) {
            self.loose.read_directory(path).await
        } else {
            Err(AssetReaderError::NotFound(path.to_path_buf()))
        }
    }

    async fn is_directory<'a>(&'a self, path: &'a Path) -> Result<bool, AssetReaderError> {
        if Self::is_public(path) {
            self.loose.is_directory(path).await
        } else {
            Ok(false)
        }
    }
}
