# Powermarks 3.5 database format (`pm35.htm`)

Recovered from `pm.exe` 3.5 build 344 (sha256 `262b2897…e702`) with REA/Ghidra 12.1.4,
cross-checked against a real 13,057-entry database. Writer: `0x421e23`. Record struct is
filled by `0x40f603`; slot offsets below are relative to that struct.

## File layout

Latin-1, CRLF. Netscape bookmark HTML with Powermarks data in comments:

```
<!DOCTYPE HTML PUBLIC "-//IETF//DTD HTML 3.2//EN">
<!--Powermarks Format 3.5-->
...
<HEAD>
<!--document=[User][<32 hex>]-->          document owner + id
<!--scs=[name][type][period hex][start time hex]-->   status-check schedules
<!--macro=[...]-->                         URL macros (formats hshhs / hshhsbb / hshhsbh)
</HEAD><BODY><DL>
<DT><A HREF="url">title</A>
<DD><!--keywords-->space separated keywords      optional
<!--notes-->...                                   optional
<!--desc-->...                                    optional
<!--user_field-->...                              optional
<!--mdata=[f0][f1]...[f14]-->
<BR>
...
</DL><p>
<!--kword=[...]-->                          tagged-keyword dictionary
</BODY></HTML>
```

One flat list: no `<H3>` folders. Keywords are the organizing mechanism.

## `mdata` fields

Type string `shhhhhhhbshhseh`: `s` string, `h` hex uint32, `b` 0/1. Times are Unix seconds in hex.

| # | Type | Slot | Meaning | Evidence |
|---|------|------|---------|----------|
| 0 | s | +0x14 | Status-check schedule name (`Weekly`) | export label "Status Checking:" |
| 1 | h | +0x24 | Date added | export label "Added:" |
| 2 | h | +0x28 | Last visited (0 = never) | export label "Last Visited:" |
| 3 | h | +0x30 | Last modified (server) | export label "Last Modified:" |
| 4 | h | +0x2c | Last checked (0 = never) | export label "Last Checked:" |
| 5 | h | +0x34 | Status timestamp (unconfirmed) | data: set on every entry, often = import time |
| 6 | h | +0x38 | Status code (unconfirmed meanings) | data: non-zero only when checked; values 0,4,7,8,9,B,C |
| 7 | h | +0x40 | Change-detection mode (0 none, 1 hash; 2/3 string) | writer switches field 9 format on it |
| 8 | b | +0x3c | Status checking enabled (unconfirmed) | always 1 in sample |
| 9 | s | +0x44 | Page content hash when f7=1, else empty | writer; 19/19 match f7=1 |
| 10 | h | +0x4c | Visit count | data: non-zero exactly where f2 set |
| 11 | h | +0x48 | Rating 0–10 (0 = not rated) | export "Rating: %d" reads +0x48 |
| 12 | s | +0x1c | Unknown string (auth user?) | always empty in sample |
| 13 | e | +0x20 | Unknown, encoded (auth password?) | always empty in sample |
| 14 | h | +0x50 | Unknown flag | 7/13,057 set |

Importers must keep all 15 raw values so an export back to this format round-trips.
Field 6 meanings come from `0x40c09a`/`0x40c179` (status → text) if they are ever needed.
