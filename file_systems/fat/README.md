# FAT (FAT12 / FAT16 / FAT32)

**Family:** FAT-chain allocation · **Origin:** Microsoft, 1977–1996 · **Status:** widely used (removable media, EFI System Partition, embedded)

One design in three variants that differ only parametrically. Represents three
entries in the file-system list (FAT12, FAT16, FAT32) but shares a single layout
and allocator. exFAT is **not** part of this — it is a different on-disk format.

## Overview

A file is a linked list of clusters: each cluster has one slot in the File
Allocation Table holding the *next* cluster number, or an end-of-chain marker.
The disk is split into a reserved/boot region, the FAT region (usually two
copies), a root-directory area, and the data region. Near-universal
interoperability makes FAT the default for small removable media and the EFI
System Partition. `DIR_FileSize` is a 32-bit field on every variant, so no FAT
file can exceed 4 GiB − 1 byte.

## Variants

| | Entry width | Max clusters | Root directory | Typical use |
|---|---|---|---|---|
| **FAT12** | 12 bits | ~4084 | fixed-size region | floppies, tiny embedded |
| **FAT16** | 16 bits | ~65524 | fixed-size region | small flash/SD, legacy DOS/Windows |
| **FAT32** | 32 bits (28 used) | ~268M | ordinary cluster chain | removable media, EFI partition |

FAT32 also adds an FSInfo sector caching the free-cluster count.

## On-disk layout (what blocks hold)

- **Reserved region** — boot sector / BPB (sector size, cluster size, FAT count); FSInfo on FAT32.
- **FAT region** — entries of the variant's width; each = next cluster or end-of-chain.
- **Root directory** — fixed region on FAT12/16; a normal chain on FAT32.
- **Data region** — file/directory cluster contents.
- A block carries: file data, a FAT entry (chain link), a directory entry, or boot/reserved.

## Format parameters

The variant is not a field — it is fixed by the entry you pick, because the
variant *is* the cluster-count band (MS spec §4). Volume size and block size
belong to the disk, not to the file system.

- Cluster (allocation unit) size.
- Number of FAT copies.
- Reserved blocks (boot sector; on FAT32 also FSInfo and the backup boot sector).
- Number of root-directory entries — FAT12/16 only, since their root is a fixed,
  un-growable region.

## References (on-disk structure)

- Spec: Microsoft "FAT32 File System Specification" (covers all three variants).
- OSDev Wiki — FAT: https://wiki.osdev.org/FAT
- Wikipedia — File Allocation Table: https://en.wikipedia.org/wiki/File_Allocation_Table
