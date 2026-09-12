# `kinds.json` — file-kind and colour table

Drives both the treemap colours and the "Kinds" sidebar. `kinds.json` in this
directory is compiled into the binary as the default; dropping a copy at
`~/.config/diskscope/kinds.json` replaces it entirely (it is not merged), so
you can recolour or reclassify without rebuilding. A malformed override is
reported in the status bar and the builtin table is used instead.

## Schema

```jsonc
{
  "$schema_version": 1,            // informational, not validated

  "folder":     { "name": "Folder",     "color": "#2b3038" },  // required
  "other":      { "name": "Other",      "color": "#6e7683" },  // required
  "free_space": { "name": "Free space", "color": "#14171c" },  // required

  // Directory extensions presented as a single block instead of being drilled
  // into. Their contents are still scanned and still counted.
  "package_extensions": ["app", "framework", "..."],

  "kinds": [                       // required, order matters
    {
      "name": "Video",             // shown in the sidebar and tooltip
      "color": "#d2691e",          // #rrggbb, base colour before cushion shading
      "extensions": ["mp4", "mov"] // lowercase, no leading dot
    }
  ]
}
```

## Rules

- **First match wins.** An extension listed under two kinds belongs to the one
  that appears first in `kinds`. The bundled file has no duplicates; keep it
  that way so the priority order never matters in practice.
- Extensions are matched case-insensitively and must be **16 bytes or shorter**
  (they are packed into a single integer for lookup speed). A longer one is a
  load error.
- Colours must be `#rrggbb`. Three-digit and named colours are rejected.
- The extension is the text after the **last** dot, so `archive.tar.gz` is
  `gz`. A name that is only a dotfile (`.gitignore`) has no extension and lands
  in `other`.
- `package_extensions` is checked before classification, so an entry there is
  never also treated as a file kind.

## Choosing colours

The treemap shades every block with a cushion (a lit 3D bump), which multiplies
the base colour by roughly 0.4–1.0. Very dark or fully saturated colours lose
their shading and read as flat, so mid-luminance, mid-saturation colours work
best — the bundled palette sits around 55–70 % lightness.
