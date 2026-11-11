import Foundation
let h = dlopen("/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote", RTLD_NOW)
typealias GetPID = @convention(c) (DispatchQueue, @escaping @convention(block) (Int32) -> Void) -> Void
typealias GetInfo = @convention(c) (DispatchQueue, @escaping @convention(block) (CFDictionary?) -> Void) -> Void
typealias Send = @convention(c) (UInt32, CFDictionary?) -> Bool
let getPid = unsafeBitCast(dlsym(h, "MRMediaRemoteGetNowPlayingApplicationPID"), to: GetPID.self)
let getInfo = unsafeBitCast(dlsym(h, "MRMediaRemoteGetNowPlayingInfo"), to: GetInfo.self)
let send = unsafeBitCast(dlsym(h, "MRMediaRemoteSendCommand"), to: Send.self)
let sem = DispatchSemaphore(value: 0)
var pid: Int32 = -1
getPid(DispatchQueue.global()) { p in pid = p; sem.signal() }
_ = sem.wait(timeout: .now() + 3)
print("nowPlayingPID=\(pid)")
getInfo(DispatchQueue.global()) { d in
  if let d = d as? [String: Any] { print("info keys=\(d.keys.sorted())"); print("title=\(d["kMRMediaRemoteNowPlayingInfoTitle"] ?? "nil")") } else { print("info=nil") }
  sem.signal()
}
_ = sem.wait(timeout: .now() + 3)
if CommandLine.arguments.count > 2, let want = Int32(CommandLine.arguments[1]), let cmd = UInt32(CommandLine.arguments[2]) {
  if pid == want { print("send(\(cmd))=\(send(cmd, nil))") } else { print("not sending: now playing app is not the target") }
}
