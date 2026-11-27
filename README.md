# ImplicitForge

A desktop SDF modeler with nine editable scenes, a parallel CPU sphere tracer and optional WGSL compute.

Windows needs the MSVC C++ build tools, macOS the Xcode command line tools, and Linux a Wayland or X11 session with xkbcommon (`libxkbcommon-x11` on X11).

The window starts on CPU. Select GPU or launch `just run --gpu` to try hardware compute. Check the adapter name in the inspector. `WGPU_BACKEND` can restrict probing to `dx12`, `metal`, `vulkan` or `gl` on a supported platform. Adapter failures leave CPU rendering available. `--cpu` disables probing.

| Action                    | Control                                                                                                                                                                      |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Choose scene              | `1`–`9` or click its name                                                                                                                                                    |
| Orbit, zoom, reset camera | Drag or arrow keys, wheel or `+` / `-`, `R`                                                                                                                                  |
| Edit a dimension          | Drag its slider, or `Tab` to select and `[` / `]` to adjust                                                                                                                  |
| Combine solids            | Open Stamp tool, enable preview, choose shape and operation, then set X/Y/Z and size. Apply keeps the edit. Undo removes the last applied stamp. Edits last for the session. |
| Compare renderers         | `G` switches compute, `B` toggles CPU pruning                                                                                                                                |
| Fullscreen                | `F11` to toggle, `Esc` to return                                                                                                                                             |
