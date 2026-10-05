# assets

`icon.png` is the master application icon and the only hand-made image here.
Everything under `generated/` and everything under `packaging/` is derived from
it by `cargo run -p icons`; nothing in either place should be edited by hand,
because the next run overwrites it.

Replacing the icon is one file and one command:

```
cp <a square 1024x1024 png> assets/icon.png
cargo run -p icons
```

The generator refuses a master that is not square or is smaller than
1024x1024. Both failures are invisible until the icon is on somebody's dock.

## Weight

The master is a rendered image rather than flat colour, so it does not
compress the way a drawn icon would: the 1024px entry in the `.icns` alone is
1.4 MB, and `packaging/` comes to 2.2 MB. That is carried in the repository,
not in the binary — the only part the binary embeds is
`generated/window-256.png`, which is 72 KB of a 20 MB budget.
