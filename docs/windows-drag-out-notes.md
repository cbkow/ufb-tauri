# Windows drag-out — OLE freeze diagnosis

**Status as of 2026-04-18:** HGLOBAL leak fixed, awaiting real-app testing to confirm the system-wide drag-drop freeze is resolved.

## Symptom (as reported)

- First file drag from UFB → Explorer / Premiere / any Windows app: works.
- Subsequent drags within the same tray-app session: system drag-and-drop is frozen **in all apps**. Even dragging in Explorer itself stops working.
- Multi-file drag: exactly one file drops, then the session wedges before the second.
- Only recovery: killing the tray app. Drag-and-drop restores immediately.

## Code path

Frontend: `src/lib/useBrowserDragDrop.ts:267` — `startNativeDrag(drag.paths)` invoked when the mouse reaches a 6px window edge margin during a drag.

Tauri command: `src-tauri/src/commands.rs:962` `start_native_drag` — dispatches via `app.run_on_main_thread(move || { ... })` to avoid blocking the async runtime, then blocks on an `mpsc::channel` for the result.

Windows implementation: `src-tauri/src/drag_out.rs:7` — custom `IDataObject` (`HDropDataObject`) + `IDropSource` (`DropSource`) using the `windows` crate (v0.58), serving `CF_HDROP` format. Calls `DoDragDrop` with `DROPEFFECT_COPY | DROPEFFECT_MOVE`.

## Root cause (confirmed)

`HDropDataObject { hmem }.into()` wraps the struct in a COM object via `#[windows::core::implement(IDataObject)]`. The COM wrapper handles refcount, but does not auto-release owned Windows handles. Without a `Drop` impl, the `HGLOBAL` from `build_cf_hdrop` was **leaked on every drag**.

Why a leaked HGLOBAL might wedge system drag-drop (not just leak memory):

1. Drop targets (Explorer's shell, WebView2, Premiere's drop handler) frequently `AddRef` the `IDataObject` and release it **asynchronously** after `DoDragDrop` returns — sometimes seconds later.
2. OLE tracks the "current drag source" via global (per-desktop) state during the drag.
3. If our `HDropDataObject` stays alive past the drag (because no `Drop` means no `GlobalFree`, and the drop target is still holding a ref), the next `DoDragDrop` fires while OLE's drag-source tracking is still pointed at the zombie object.
4. Result: subsequent drags appear to "start" but never fully initialize → system-wide freeze until the process exits and Windows reclaims the leaked handles.

## Fix applied

`src-tauri/src/drag_out.rs`:

```rust
impl Drop for HDropDataObject {
    fn drop(&mut self) {
        unsafe {
            if !self.hmem.is_invalid() {
                let _ = GlobalFree(self.hmem);
            }
        }
    }
}
```

## Why I'm only *likely*, not certain, this is the fix

The HGLOBAL leak is definitively a bug and the fix removes it. The *system-wide freeze symptom* is plausibly explained by the leak causing OLE drag-source state to linger, but I haven't reproduced it in a minimal harness to prove the exact mechanism. Windows OLE drag-drop has historically had several distinct causes for this symptom:

- Leaked COM refs holding OLE drag state (this fix addresses this)
- WebView2 / webview drag-drop registration conflicting with our `DoDragDrop` on the same HWND
- Missing `IDataObjectAsyncCapability` forcing Explorer into sync-copy mode that blocks past our return
- `DoDragDrop` called from a thread that wasn't properly STA-initialized

## Test plan before calling this fixed

On Windows:

1. Build and run the tray app
2. Drag a single file from UFB to Explorer, confirm drop
3. Drag a second file from UFB to Explorer, confirm drop — **this is where it used to freeze**
4. Drag a batch of 5 files from UFB to Explorer, confirm all 5 land
5. Drag from UFB to a non-shell target: Chrome, Premiere, AE
6. After each step, drag a file within Explorer itself to confirm system drag-drop still works
7. Close and reopen the tray app after 10+ drags, verify no resource warnings in logs

Pass criterion: no frozen drag-drop anywhere on the system after an extended drag session. If it fails anywhere, see fallback suspects below.

## Fallback suspects if the fix is insufficient

In priority order (most likely first):

### 1. WebView2 / HWND drag-drop registration conflict

WebView2 registers its own OLE drop target on the webview's HWND so users can drag files *into* the app. Our `DoDragDrop` starts a drag *out* from the same HWND. OLE may get confused between "I'm a drag source" and "I'm a drop target" on the same window.

Possible fix: revoke the webview's drop target before `DoDragDrop`, re-register after:

```rust
RevokeDragDrop(webview_hwnd);
let hr = DoDragDrop(...);
RegisterDragDrop(webview_hwnd, stashed_webview_drop_target);
```

The sharp edge: getting the webview's current `IDropTarget` pointer requires either Tauri API support or poking at the WebView2 internals. Could be messy.

### 2. Missing `IDataObjectAsyncCapability`

Explorer's shell copy handler queries for `IDataObjectAsyncCapability`. If supported, it runs the copy on its own thread. If not, it synchronously copies on our thread — holding refs to our `IDataObject` until done.

Fix: implement `IDataObjectAsyncCapability` alongside `IDataObject`. `SetAsyncMode(TRUE)` / `GetAsyncMode` / `StartOperation` / `InOperation` / `EndOperation`. Mostly stubs that return S_OK — the interface's existence is enough to opt into async copy.

### 3. Run DoDragDrop on a dedicated STA thread

Currently we dispatch to Tauri's main thread via `app.run_on_main_thread`. Tauri's main thread owns WebView2; mixing our drag source with the webview's drop target on the same thread may be the conflict.

Fix: spawn a dedicated OS thread, `CoInitialize(STA)` on it, run `DoDragDrop` there. Cross-thread COM is involved but `IDataObject` is apartment-threaded and the `windows` crate handles the marshaling.

## References

- Microsoft docs on [DoDragDrop](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop) (OLE drag-drop, canonical source behavior)
- [IDataObject::GetData](https://learn.microsoft.com/en-us/windows/win32/api/objidl/nf-objidl-idataobject-getdata) — STGMEDIUM ownership rules
- [IDataObjectAsyncCapability](https://learn.microsoft.com/en-us/windows/win32/api/shldisp/nn-shldisp-idataobjectasynccapability) — for the fallback suspect
