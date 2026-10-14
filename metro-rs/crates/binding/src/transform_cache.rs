//! The `transform-cache` component: metro-cache's `FileStore.get` reads an
//! entry with one read on a worker thread, instead of `fs.promises.readFile`'s
//! open, stat, read and close round trips. Parsing stays in JS (V8's
//! `JSON.parse`), and transform outputs stay JS objects (rule 10, decided in
//! port/design/phase-4.md).

use metro_rs_file_map::FileMapError;
use napi::bindgen_prelude::AsyncTask;
use napi::{Env, JsUnknown, Result, Task};
use napi_derive::napi;

pub struct ReadEntryTask {
    path: String,
}

pub enum ReadEntryOutcome {
    Found(Vec<u8>),
    NotFound,
    Failed(FileMapError),
}

impl Task for ReadEntryTask {
    type Output = ReadEntryOutcome;
    type JsValue = JsUnknown;

    fn compute(&mut self) -> Result<ReadEntryOutcome> {
        use std::io::Read;
        // Open and read separately, so a failure names the same syscall as
        // Node's `readFile` does (e.g. a directory fails in `read`).
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReadEntryOutcome::NotFound);
            }
            Err(error) => {
                return Ok(ReadEntryOutcome::Failed(FileMapError::from_io(
                    &error, "open", &self.path,
                )));
            }
        };
        let mut content = Vec::new();
        Ok(match file.read_to_end(&mut content) {
            Ok(_) => ReadEntryOutcome::Found(content),
            Err(error) => {
                ReadEntryOutcome::Failed(FileMapError::from_io(&error, "read", &self.path))
            }
        })
    }

    fn resolve(&mut self, env: Env, output: ReadEntryOutcome) -> Result<JsUnknown> {
        match output {
            ReadEntryOutcome::Found(content) => Ok(env
                .create_buffer_with_data(content)?
                .into_raw()
                .into_unknown()),
            ReadEntryOutcome::NotFound => Ok(env.get_null()?.into_unknown()),
            ReadEntryOutcome::Failed(error) => {
                let mut object = env.create_object()?;
                if let Some(code) = &error.code {
                    object.set_named_property("errorCode", env.create_string(code)?)?;
                }
                object.set_named_property("errorMessage", env.create_string(&error.message)?)?;
                Ok(object.into_unknown())
            }
        }
    }
}

/// Reads one transform-cache entry: a `Buffer`, `null` when the file does not
/// exist (`ENOENT`), or `{errorCode, errorMessage}` for any other failure,
/// which the JS side throws as Node would.
#[napi]
pub fn transform_cache_read(path: String) -> AsyncTask<ReadEntryTask> {
    AsyncTask::new(ReadEntryTask { path })
}
