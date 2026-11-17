# Bundled fonts

All hosts register this same offline font stack through `init_bundled`.
Custom themes may override the selected families.

- IBM Plex Sans: regular (400), medium (500), semibold (600), bold (700),
  and each matching italic. Unmodified complete TTF files from
  [IBM/plex](https://github.com/IBM/plex/tree/763c36ef9117782905ae010056dfbe8fd2653a25/packages/plex-sans/fonts/complete/ttf),
  revision `763c36ef9117782905ae010056dfbe8fd2653a25`.
- JetBrains Mono: regular, unmodified file from the GPUI Kit gallery at
  revision `201b55a431fb1b82a6047e908de63913db3d4354`.

The original files retain their font metadata and copyrights and use the
[SIL Open Font License](OFL.txt). Register all UI styles before opening windows;
web has no native font inventory to supply absent weights or italics.
