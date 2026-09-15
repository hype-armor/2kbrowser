# assets

`icon.png` is the master application icon and the only hand-made image here.
Everything under `generated/` and everything under `packaging/` is derived from
it by `cargo run -p icons`; nothing in either place should be edited by hand,
because the next run will overwrite it.

## The current master is a placeholder

It is a drawn approximation — the right silhouette and palette, no more — put
here so the generator could be built, run and looked at before the real
artwork existed. It is deliberately close enough to judge the derived sizes and
deliberately not the finished icon.

Replacing it is one file and one command:

```
cp <the real 1024x1024 png> assets/icon.png
cargo run -p icons
```

The generator refuses a master smaller than 1024x1024 or one that is not
square, because both failures are invisible until the icon is on somebody's
dock.
