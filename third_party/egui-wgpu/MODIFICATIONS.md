# Changes relative to egui-wgpu 0.37.0 (MIT OR Apache-2.0)

Made for graineo (November 2026). Everything else is unchanged.

- `src/lib.rs`: `PREFER_FLOAT_SURFACE` (static flag) – `preferred_framebuffer_format` then picks
  `Rgba16Float` if the surface offers it. egui still uses its gamma-space shader path (the format is not
  sRGB), so UI colours and blending are the same as on an 8-bit surface.
- `src/winit.rs`: a `Rgba16Float` surface is configured with `SurfaceColorSpace::ExtendedSrgb`
  (sRGB-encoded values, below 0 / above 1 allowed: colours outside sRGB and HDR highlights).
- `src/renderer.rs`: `Renderer::register_photo_texture` (a native texture marked as photo, optionally
  HDR) and `Renderer::set_photo_proof` (colour proof as part of the uniform buffer); the texture flags get
  two more bits (photo, HDR), `NUM_TEXTURE_FLAGS` 6 → 32.
- `src/egui.wgsl`: `Locals.photo_proof` and `photo_proof()`: for photo textures only, limit to a proof
  colour space and/or mark pixels outside it (soft proof, gamut warning); other textures pass unchanged.
- `src/capture.rs`: screenshots of a `Rgba16Float` surface (clamped to 8 bit for the image; the
  unclamped values are kept in `LAST_FLOAT_CAPTURE` for measurements).
