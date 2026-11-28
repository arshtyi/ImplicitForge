# ImplicitForge

A desktop SDF modeler with nine editable scenes, a parallel CPU sphere tracer and optional WGSL compute.

Windows needs the MSVC C++ build tools, macOS the Xcode command line tools, and Linux a Wayland or X11 session with xkbcommon (`libxkbcommon-x11` on X11).

The window starts on CPU. Select GPU or launch `just run --gpu` to try hardware compute. Check the adapter name in the inspector. `WGPU_BACKEND` can restrict probing to `dx12`, `metal`, `vulkan` or `gl` on a supported platform. Adapter failures leave CPU rendering available. `--cpu` disables probing.

| Action                    | Control                                                                                                                                                                                           |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Choose scene              | `1`–`9` or click its name                                                                                                                                                                         |
| Orbit, zoom, reset camera | Drag or arrow keys, wheel or `+` / `-`, `R`                                                                                                                                                       |
| Edit a dimension          | Drag its slider, or `Tab` to select and `[` / `]` to adjust                                                                                                                                       |
| Edit stamps               | In Stamp tool, cycle shape and operation. Switch Dimensions / Position to adjust the selected tool. Axis aligns local Y to world X, Y or Z. Preview, Apply and Undo operate on the current scene. |
| Compare renderers         | `G` switches compute, `B` toggles CPU pruning                                                                                                                                                     |
| Primitive costs           | Cost map or `H` shows CPU primitive calls per pixel. At the same Target view, `B` compares full and pruned queries. Select GPU to return to shading.                                              |
| Fullscreen                | `F11` to toggle, `Esc` to return                                                                                                                                                                  |
