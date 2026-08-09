#!/usr/bin/env python3
"""一次性 SG_IO 探针：直接查看 LOCATE/READ 的原始返回，用于调试 LTFS 读取。

用法（在磁带服务器上）：
    python3 sg_probe.py /dev/sg1

只做读取操作，不修改磁带内容。
"""

import ctypes
import os
import struct
import sys
import time

SG_IO = 0x2285
SG_DXFER_NONE = -1
SG_DXFER_FROM_DEV = -3


class SgIoHdr(ctypes.Structure):
    _fields_ = [
        ("interface_id", ctypes.c_int),
        ("dxfer_direction", ctypes.c_int),
        ("cmd_len", ctypes.c_ubyte),
        ("mx_sb_len", ctypes.c_ubyte),
        ("iovec_count", ctypes.c_ushort),
        ("dxfer_len", ctypes.c_uint),
        ("dxferp", ctypes.c_void_p),
        ("cmdp", ctypes.c_void_p),
        ("sbp", ctypes.c_void_p),
        ("timeout", ctypes.c_uint),
        ("flags", ctypes.c_uint),
        ("pack_id", ctypes.c_int),
        ("usr_ptr", ctypes.c_void_p),
        ("status", ctypes.c_ubyte),
        ("masked_status", ctypes.c_ubyte),
        ("msg_status", ctypes.c_ubyte),
        ("sb_len_wr", ctypes.c_ubyte),
        ("host_status", ctypes.c_ushort),
        ("driver_status", ctypes.c_ushort),
        ("resid", ctypes.c_int),
        ("duration", ctypes.c_uint),
        ("info", ctypes.c_uint),
    ]


def sg_io(fd, cdb, buflen=0, direction=SG_DXFER_FROM_DEV):
    buf = ctypes.create_string_buffer(b"\x00" * buflen) if buflen else None
    sense = ctypes.create_string_buffer(32)
    cdb_buf = ctypes.create_string_buffer(bytes(cdb))
    hdr = SgIoHdr()
    hdr.interface_id = 0x53
    hdr.dxfer_direction = direction
    hdr.cmd_len = len(cdb)
    hdr.mx_sb_len = 32
    hdr.dxfer_len = buflen
    hdr.dxferp = ctypes.cast(buf, ctypes.c_void_p) if buf else None
    hdr.cmdp = ctypes.cast(cdb_buf, ctypes.c_void_p)
    hdr.sbp = ctypes.cast(sense, ctypes.c_void_p)
    hdr.timeout = 15000
    libc = ctypes.CDLL(None, use_errno=True)
    t0 = time.monotonic()
    rc = libc.ioctl(ctypes.c_int(fd), SG_IO, ctypes.byref(hdr))
    elapsed_ms = (time.monotonic() - t0) * 1000
    data = bytes(buf.raw[:buflen]) if buf else b""
    return {
        "rc": rc,
        "status": hdr.status,
        "host_status": hdr.host_status,
        "driver_status": hdr.driver_status,
        "resid": hdr.resid,
        "sb_len_wr": hdr.sb_len_wr,
        "sense": bytes(sense.raw[: hdr.sb_len_wr]),
        "data_len": len(data),
        "data_head": data[:80],
        "data": data,
        "ms": elapsed_ms,
    }


def show(tag, r):
    print(f"{tag}: {r['ms']:.0f}ms rc={r['rc']} status=0x{r['status']:02x} resid={r['resid']} "
          f"data_len={r['data_len']}")
    if r["sense"]:
        print(f"  sense: {r['sense'].hex()}")
    if r["data_head"]:
        print(f"  head: {r['data_head'][:40]!r}")


def main():
    dev = sys.argv[1] if len(sys.argv) > 1 else "/dev/sg1"
    fd = os.open(dev, os.O_RDWR | os.O_NONBLOCK)

    # LOCATE(16) 到分区 0 块 0（CP 置位，允许跨分区）
    locate = bytes([0x92, 0x02, 0, 0]) + struct.pack(">Q", 0) + bytes(4)
    show("locate p0 b0", sg_io(fd, list(locate), 0, SG_DXFER_NONE))

    # READ(6) 可变块，1 MiB
    read = bytes([0x08, 0x00, 0x10, 0x00, 0x00, 0x00])
    show("read", sg_io(fd, list(read), 1024 * 1024))

    # 依次读取 label 序列：[VOL1][FM][XML label][FM]
    for i in range(4):
        r = sg_io(fd, list(read), 1024 * 1024)
        print(f"read{i + 1}: rc={r['rc']} status=0x{r['status']:02x} resid={r['resid']} "
              f"data_len={r['data_len']}")
        if r["sense"]:
            print(f"  sense: {r['sense'].hex()}")
        if r["data_head"]:
            print(f"  head: {r['data_head'][:80]!r}")
        if r["resid"] < 1024 * 1024 and r["data_head"].startswith(b"<?xml"):
            n = 1024 * 1024 - r["resid"]
            with open("/tmp/ltfslabel.xml", "wb") as f:
                f.write(r["data"][:n])
                print(f"  -> saved /tmp/ltfslabel.xml ({n} bytes)")

    # READ POSITION
    show("readpos", sg_io(fd, [0x34, 0x06] + [0] * 8, 32))

    # ---- 模拟 tapecpy volume 的完整路径并计时 ----
    # 分区 1 探测（跨分区定位）
    locate_p1 = bytes([0x92, 0x02, 0, 1]) + struct.pack(">Q", 0) + bytes(4)
    show("locate p1 b0 (CP)", sg_io(fd, list(locate_p1), 0, SG_DXFER_NONE))
    for i in range(4):
        show(f"p1 read{i + 1}", sg_io(fd, list(read), 1024 * 1024))

    # 回到 index 分区（分区 0）块 4 扫描到 EOD
    locate_p0b4 = bytes([0x92, 0x02, 0, 0]) + struct.pack(">Q", 4) + bytes(4)
    show("locate p0 b4 (CP)", sg_io(fd, list(locate_p0b4), 0, SG_DXFER_NONE))
    for i in range(8):
        r = sg_io(fd, list(read), 1024 * 1024)
        fm = " FM" if r["sense"] and (r["sense"][2] & 0x80) else ""
        eod = " EOD" if r["sense"] and (r["sense"][2] & 0x0f) == 0x08 else ""
        print(f"scan read{i + 1}: {r['ms']:.0f}ms resid={r['resid']}{fm}{eod}")
        if r["data_head"].startswith(b"<?xml"):
            n = 1024 * 1024 - r["resid"]
            with open("/tmp/ltfsindex.xml", "wb") as f:
                f.write(r["data"][:n])
            print(f"  -> saved /tmp/ltfsindex.xml ({n} bytes)")
        if eod:
            break

    os.close(fd)


if __name__ == "__main__":
    main()
