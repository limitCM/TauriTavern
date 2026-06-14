# iOS Agent Background Activity

TauriTavern wraps each Agent run in an iOS background activity when the app is
running on iOS.

## Runtime Strategy

When an Agent run starts, the runtime attempts this order:

1. On iOS 26 or newer signed builds that enable the
   `ios-bg-continued-processing` Cargo feature, submit a
   `BGContinuedProcessingTaskRequest` for the run.
2. If continued processing is unavailable or submission fails, fall back to
   `UIApplication.beginBackgroundTask`.
3. If both mechanisms are unavailable, continue the Agent run without a native
   background activity and record an Agent event.

The continued-processing bridge uses dynamic Objective-C lookup for the iOS 26
classes and selectors. This keeps older SDK/device paths on the fallback route
instead of requiring compile-time iOS 26 symbols.

Unsigned/sideload smoke-test builds keep continued processing disabled by
default because `BGTaskScheduler` can require capabilities that local resigning
tools do not provide. Those builds use the finite background-task fallback.

The continued-processing launch handler is registered during Tauri app setup
only when the Cargo feature is enabled. Agent runs only submit per-run task
requests after they start.

## Progress And Cancellation

Agent status transitions update the native continued-processing `NSProgress`
object when a continued-processing task has been launched by the system.

Approximate progress is mapped from run status:

```text
initializing workspace     10%
assembling context         20%
calling model              40%
dispatching tools          60%
checkpoint / host commit   76-82%
finishing                  92%
terminal state             100%
```

If iOS expires or cancels the background activity, TauriTavern:

- sends the Agent cancellation signal,
- writes an `ios_background_expiration` checkpoint,
- cancels unfinished child tasks,
- clears pending host requests,
- transitions the run toward cancellation if it is still non-terminal.

## Agent Events

The run journal records these iOS background events:

```text
ios_agent_background_activity_started
ios_agent_background_activity_unavailable
ios_agent_background_activity_expired
ios_agent_background_activity_finished
checkpoint_created
```

The `started` event includes the selected mode:

```text
continued_processing
finite_background_task
```

## iOS Project Configuration

The iOS project declares the continued-processing task identifier pattern in:

```text
src-tauri/Info.ios.plist
src-tauri/gen/apple/tauritavern_iOS/Info.plist
src-tauri/gen/apple/project.yml
```

The generated Xcode project links:

```text
BackgroundTasks.framework
```

## Verification

Useful checks after pushing this branch to GitHub:

1. Run the `iOS IPA Build` workflow on a macOS runner.
2. Inspect the build log for Rust compile errors in
   `ios_agent_background.rs`.
3. Install the IPA on an iOS device.
4. Start an Agent run and inspect run events for
   `ios_agent_background_activity_started`.
5. On unsigned/sideload builds, confirm the fallback mode is
   `finite_background_task`. On signed iOS 26+ builds with
   `ios-bg-continued-processing`, confirm the mode is `continued_processing`.
