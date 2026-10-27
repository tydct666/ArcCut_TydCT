#![cfg(all(windows, feature = "resource-test-hooks"))]
//! Explicit bounded real-media regression; counters are simulated downward,
//! never exhaust this machine's memory or alter Windows configuration.
use arkcut_core::{Engine, resource::{GIB, MIB}};
use serde_json::{json, Value};
use std::{fs, path::Path, time::{Duration, Instant}};
fn state(e:&Engine)->Value { e.request("status", json!({"includeResult":true})).unwrap() }
fn done(s:&Value)->bool { !matches!(s["job"]["status"].as_str(),Some("running"|"cancelling")) }
fn wait(e:&Engine)->Value {
 let start=Instant::now();loop {let s=state(e);if done(&s){return s}if start.elapsed()>Duration::from_secs(900){let _=e.request("cancel",json!({}));panic!("test timeout")};std::thread::sleep(Duration::from_millis(50));}
}
fn write(p:&Path,v:&Value){fs::write(p,serde_json::to_vec_pretty(v).unwrap()).unwrap();}
fn hook(p:&Path,v:Value){let temp=p.with_extension("next");write(&temp,&v);fs::rename(temp,p).unwrap();}
#[test]
#[ignore="Explicit real-media test; requires ARKCUT_ADAPTIVE_PROJECT and ARKCUT_ADAPTIVE_OUT"]
fn low_capacity_preview_failure_cancel_and_recovery(){
 let out=std::path::PathBuf::from(std::env::var_os("ARKCUT_ADAPTIVE_OUT").unwrap());fs::create_dir(&out).unwrap();
 let data=out.join("data");fs::create_dir(&data).unwrap();std::env::set_var("ARKCUT_DATA_DIR",&data);
 let hooks=data.join(".arkcut-resource-test.json");std::env::set_var("ARKCUT_RESOURCE_TEST_FILE",&hooks);hook(&hooks,json!({}));
 let project:Value=serde_json::from_slice(&fs::read(std::env::var_os("ARKCUT_ADAPTIVE_PROJECT").unwrap()).unwrap()).unwrap();
 let engine=Engine::default();let mut results=vec![];
 // Wait for one genuine ready clip, then create a critical signal. Previously
 // the backend mislabeled every pending clip as user-cancelled here.
 engine.request("previewBatch",json!({"project":project,"halfSeconds":3})).unwrap();
 let start=Instant::now();loop {let s=state(&engine);assert!(!done(&s),"{s}");if s["job"]["previews"].as_array().unwrap().iter().any(|v|v["status"]=="ready"){break}assert!(start.elapsed()<Duration::from_secs(180));std::thread::sleep(Duration::from_millis(50));}
 hook(&hooks,json!({"availableBytes":256*MIB}));let failed=wait(&engine);
 assert_eq!(failed["job"]["status"],"failed");assert!(failed["job"]["error"].as_str().unwrap().starts_with("RESOURCE_MEMORY"));
 let entries=failed["job"]["previews"].as_array().unwrap();assert!(entries.iter().any(|v|v["status"]=="ready"));assert!(entries.iter().any(|v|v["status"]=="failed"));assert!(entries.iter().all(|v|v["status"]!="cancelled"));
 write(&out.join("critical-failure.json"),&failed);results.push(json!({"case":"critical failure retains ready media and reports failed","passed":true}));
 let confirmed:Vec<_>=project["pauses"].as_array().unwrap().iter().filter(|p|p["enabled"]==true&&p["state"]=="confirmed").cloned().collect();
 for (label,total,available,cpus) in [("4GiB-2CPU",4*GIB,1536*MIB,2),("8GiB-4CPU",8*GIB,2*GIB,4)] {
  hook(&hooks,json!({"totalBytes":total,"availableBytes":available,"commitAvailableBytes":4*GIB,"logicalProcessors":cpus}));
  let mut p=project.clone();p["projectId"]=json!(uuid::Uuid::new_v4().to_string());
  // Exercise every clip at the smaller live headroom, not only easy endpoints.
  let expected=if total==4*GIB {p["pauses"]=json!(confirmed);confirmed.len()} else {
   p["pauses"]=json!([confirmed.first().unwrap(),confirmed.last().unwrap()]);2
  };
  engine.request("previewBatch",json!({"project":p,"halfSeconds":3})).unwrap();let result=wait(&engine);
  write(&out.join(format!("{label}.json")),&result);assert_eq!(result["job"]["status"],"completed","{result}");
  assert_eq!(result["job"]["result"]["ready"],expected);assert_eq!(result["job"]["result"]["total"],expected);assert_eq!(result["job"]["resource"]["simulation"],true);
  assert!(result["job"]["result"]["previews"].as_array().unwrap().iter().all(|x|x["status"]=="ready"));
  assert!(result["job"]["resource"]["budgetDecisions"].as_array().unwrap().iter().any(|v|v["advisoryFallback"]==true));
  results.push(json!({"case":label,"passed":true,"simulation":true,"ready":expected,"total":expected,"resource":result["job"]["resource"]}));
 }
 hook(&hooks,json!({}));let mut p=project.clone();p["projectId"]=json!(uuid::Uuid::new_v4().to_string());p["pauses"]=json!([confirmed.first().unwrap()]);
 engine.request("previewBatch",json!({"project":p,"halfSeconds":3})).unwrap();
 let start=Instant::now();loop {let s=state(&engine);assert!(!done(&s));if s["job"]["phase"]=="previewRender"{break}assert!(start.elapsed()<Duration::from_secs(180));std::thread::sleep(Duration::from_millis(20));}
 let start=Instant::now();engine.request("cancel",json!({})).unwrap();let cancelled=wait(&engine);let ms=start.elapsed().as_millis();assert_eq!(cancelled["job"]["status"],"cancelled");assert!(ms<5000);write(&out.join("cancel.json"),&cancelled);
 results.push(json!({"case":"cancel preview render","passed":true,"responseMs":ms}));
 engine.request("previewBatch",json!({"project":p,"halfSeconds":3})).unwrap();let recovered=wait(&engine);assert_eq!(recovered["job"]["status"],"completed");write(&out.join("recovered.json"),&recovered);
 results.push(json!({"case":"same Engine recovers after failure and cancellation","passed":true}));
 write(&out.join("evidence.json"),&json!({"status":"passed","simulation":true,"realLowRamHardware":false,"cases":results}));
}
