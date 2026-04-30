use bytes::Bytes;
use slatedb::object_store::{ObjectStore, path::Path};

pub struct Fetcher {
    object_store: Box<dyn ObjectStore>,
}

impl Fetcher {
    pub fn new(object_store: Box<dyn ObjectStore>) -> Self {
        Self {
            object_store,
        }
    }

    pub async fn fetch_wasm_bytes(&self, version: &str, service_name: &str) -> Result<Option<Bytes>, Box<dyn std::error::Error>> {
        let path = Path::from(format!("{}/{}", version, service_name));
        match self.object_store.get(&path).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(slatedb::object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
