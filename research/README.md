# Why the app doesn't copy through the OS

`kernel_vfat.py` measured what order the real Linux vfat driver leaves when a
playlist is copied in order onto sticks with a history (an earlier copy
overwritten, an existing folder with deleted files, the top level of a used
stick). The raw order came out wrong in 4 of 7 cases; see
`kernel-vfat-results.json` and the console log.

It drives the `fatstick copy` and `fatstick sort` commands of the earlier copy-then-reorder
prototype (commit 93f9b75), which no longer exist: the app now erases the stick
and writes a fresh FAT32 file system itself. Check out that commit to rerun it.
