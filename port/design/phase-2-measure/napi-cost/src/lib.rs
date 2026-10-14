use napi::{Env, JsFunction, JsObject, JsString, JsUnknown, Result, ValueType};
use napi_derive::napi;
use std::time::Instant;

fn read_str(o: &JsObject, k: &str) -> Result<String> {
  o.get_named_property::<JsString>(k)?.into_utf8()?.into_owned()
}

/// Each returns ns per call over `n` calls with `paths` cycled.
#[napi]
pub fn empty_call(f: JsFunction, n: u32) -> Result<f64> {
  let t = Instant::now();
  for _ in 0..n { f.call_without_args(None)?; }
  Ok(t.elapsed().as_nanos() as f64 / n as f64)
}

#[napi]
pub fn lookup_calls(env: Env, f: JsFunction, paths: Vec<String>, n: u32) -> Result<f64> {
  let args: Vec<JsString> = paths.iter().map(|p| env.create_string(p)).collect::<Result<_>>()?;
  let _ = &args;
  let t = Instant::now();
  let mut acc = 0usize;
  for i in 0..n as usize {
    let p = env.create_string(&paths[i % paths.len()])?;
    let r: JsObject = f.call(None, &[p])?.coerce_to_object()?;
    let exists: bool = r.get_named_property::<napi::JsBoolean>("exists")?.get_value()?;
    if exists {
      let ty = read_str(&r, "type")?;
      let real = read_str(&r, "realPath")?;
      acc += ty.len() + real.len();
    }
  }
  std::hint::black_box(acc);
  Ok(t.elapsed().as_nanos() as f64 / n as f64)
}

#[napi]
pub fn string_or_null_calls(env: Env, f: JsFunction, paths: Vec<String>, n: u32) -> Result<f64> {
  let t = Instant::now();
  let mut acc = 0usize;
  for i in 0..n as usize {
    let p = env.create_string(&paths[i % paths.len()])?;
    let r: JsUnknown = f.call(None, &[p])?;
    if r.get_type()? == ValueType::String {
      acc += unsafe { r.cast::<JsString>() }.into_utf8()?.into_owned()?.len();
    }
  }
  std::hint::black_box(acc);
  Ok(t.elapsed().as_nanos() as f64 / n as f64)
}

/// getPackageForModule: read rootPath + packageRelativePath, and the package.json
/// either (a) as a handle only, (b) via JSON.stringify + serde_json.
#[napi]
pub fn package_calls(env: Env, f: JsFunction, stringify: JsFunction, paths: Vec<String>, n: u32, convert: bool) -> Result<f64> {
  let t = Instant::now();
  let mut acc = 0usize;
  for i in 0..n as usize {
    let p = env.create_string(&paths[i % paths.len()])?;
    let r: JsUnknown = f.call(None, &[p])?;
    if r.get_type()? == ValueType::Object {
      let o: JsObject = r.coerce_to_object()?;
      acc += read_str(&o, "rootPath")?.len() + read_str(&o, "packageRelativePath")?.len();
      let pj: JsUnknown = o.get_named_property("packageJson")?;
      if convert {
        let s: JsString = stringify.call(None, &[pj])?.coerce_to_string()?;
        let s = s.into_utf8()?.into_owned()?;
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        acc += v.as_object().map(|m| m.len()).unwrap_or(0);
      }
    }
  }
  std::hint::black_box(acc);
  Ok(t.elapsed().as_nanos() as f64 / n as f64)
}

/// JS -> Rust entry: read the non-callback context fields a resolve() needs.
#[napi]
pub fn read_context(ctx: JsObject) -> Result<u32> {
  let mut acc = 0u32;
  for k in ["originModulePath"] { acc += read_str(&ctx, k)?.len() as u32; }
  for k in ["allowHaste", "dev", "disableHierarchicalLookup", "preferNativePlatform", "unstable_enablePackageExports", "isESMImport"] {
    let v: JsUnknown = ctx.get_named_property(k)?; acc += (v.get_type()? == ValueType::Boolean) as u32;
  }
  for k in ["mainFields", "sourceExts", "nodeModulesPaths", "unstable_conditionNames"] {
    let a: JsObject = ctx.get_named_property(k)?; let len = a.get_array_length()?;
    for i in 0..len { let s: JsString = a.get_element(i)?; acc += s.into_utf8()?.as_str()?.len() as u32; }
  }
  for k in ["fileSystemLookup", "getPackageForModule", "getPackage", "doesFileExist", "resolveHasteModule", "resolveHastePackage", "resolveAsset", "unstable_logWarning"] {
    let v: JsUnknown = ctx.get_named_property(k)?; acc += (v.get_type()? == ValueType::Function) as u32;
  }
  Ok(acc)
}

#[napi]
pub fn noop(_s: String) -> u32 { 0 }
