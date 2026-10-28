# Vendored: Signalsmith Stretch 1.3.1 and Signalsmith Linear

Pitch and time stretching by Geraint Luff / Signalsmith Audio, MIT licence
(see each folder's `LICENSE.txt`). Upstream:

- https://github.com/Signalsmith-Audio/signalsmith-stretch
- https://github.com/Signalsmith-Audio/linear

Taken as packaged in the `signalsmith-stretch` crate 0.1.3 (MIT, Colin Marc),
whose C wrapper `../src/wrapper.cpp` is adapted from. The web demo, command
line tool and tests are left out. Only headers: they're compiled into the
app by `../build.rs`.
