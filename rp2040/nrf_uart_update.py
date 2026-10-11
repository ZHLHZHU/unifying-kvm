#!/usr/bin/env python3
"""Run on Linux VM. Connect CMSIS-DAP to RP2040, power the whole PCB via USB-C."""
import argparse, binascii, pathlib, re, socket, struct, subprocess, tempfile, time

def prepare_image(path):
    path=pathlib.Path(path)
    if path.read_bytes()[:4] == b'\x7fELF':
        with tempfile.TemporaryDirectory(prefix='nrf-uart-') as tmp:
            out=pathlib.Path(tmp)/'app.bin'
            subprocess.run(['arm-none-eabi-objcopy','-O','binary','--only-section=.vector_table','--only-section=.text','--only-section=.rodata','--only-section=.data',str(path),str(out)],check=True)
            data=out.read_bytes()
    else: data=path.read_bytes()
    data+=b'\xff'*(-len(data)%4)
    if not 256 <= len(data) <= 480*1024: raise ValueError('Invalid image size')
    sp,reset=struct.unpack('<II',data[:8])
    if not 0x20000000<=sp<=0x20040000 or sp%8 or not reset&1 or not 0x7100<=reset&~1<0x7000+len(data): raise ValueError('Not an app linked at 0x7000')
    if b'OTA UART1 BLOCK64 CRC32' not in data: raise ValueError('Image lacks UART OTA support; refusing to remove the update path')
    build=re.search(rb'VER unifying/1 build=(\d+)',data)
    if not build: raise ValueError('Image has no build identity')
    return data,build.group(1).decode()

class Tcl:
    def __init__(self,port):
        self.sock=socket.create_connection(('127.0.0.1',port),2);self.sock.settimeout(8)
    def cmd(self,s):
        self.sock.sendall(s.encode()+b'\x1a');b=b''
        while b'\x1a' not in b:
            c=self.sock.recv(65536)
            if not c:raise RuntimeError('OpenOCD disconnected')
            b+=c
        result=b.split(b'\x1a')[0].decode()
        if 'error' in result.lower(): raise RuntimeError(result)
        return result
    def read(self,addr,n):return [int(x,0) for x in self.cmd(f'read_memory {addr:#x} 32 {n}').split()]
    def write(self,addr,words):self.cmd(f'write_memory {addr:#x} 32 {{'+ ' '.join(str(x) for x in words)+'}')

class Relay:
    def __init__(self,tcl,addr):
        self.t,self.a=tcl,addr
        if self.t.read(addr,2)!=[0x4f544131,1]:raise RuntimeError('RP firmware lacks OTA mailbox version 1')
        self.t.write(addr+12,[0]);self.t.write(addr+8,[1]);self.wait()
    def wait(self):
        deadline=time.monotonic()+35
        while time.monotonic()<deadline:
            status=self.t.read(self.a+12,1)[0]
            if status in (3,4):
                n=self.t.read(self.a+20,1)[0]
                if n>600:raise RuntimeError('Invalid mailbox response length')
                data=b''.join(struct.pack('<I',x) for x in self.t.read(self.a+280,(n+3)//4))[:n]
                if status==4:raise RuntimeError(data.decode(errors='replace'))
                return data
            time.sleep(.01)
        raise RuntimeError('RP relay did not finish')
    def exchange(self,command):
        b=command.encode();b+=b'\0'*(-len(b)%4)
        self.t.write(self.a+24,list(struct.unpack('<'+'I'*(len(b)//4),b)))
        self.t.write(self.a+16,[len(command)]);self.t.write(self.a+12,[0]);self.t.write(self.a+8,[2])
        reply=self.wait().decode(errors='replace').strip()
        if not re.match(r'^@[0-9A-F]{8} ',reply):raise RuntimeError('Malformed UART reply: '+reply)
        return reply[10:]
    def reset(self):self.t.cmd('reset run')

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rp-elf',required=True);p.add_argument('--probe-serial');p.add_argument('--port',type=int,default=16666)
    g=p.add_mutually_exclusive_group();g.add_argument('--image');g.add_argument('--command',help='send one nRF UART command, e.g. "UPAIR 01"')
    a=p.parse_args();image,build=prepare_image(a.image) if a.image else (None,None)
    nm=subprocess.check_output(['arm-none-eabi-nm',a.rp_elf],text=True)
    match=re.search(r'^([0-9a-fA-F]+) \w rp_ota_mailbox$',nm,re.M)
    if not match:raise RuntimeError('Mailbox missing from supplied RP ELF')
    addr=int(match[1],16)
    cmd=['openocd','-f','interface/cmsis-dap.cfg','-f','target/rp2040.cfg','-c','adapter speed 1000','-c','bindto 127.0.0.1','-c',f'tcl_port {a.port}','-c','gdb_port disabled','-c','telnet_port disabled']
    if a.probe_serial:cmd+=['-c','cmsis_dap_serial '+a.probe_serial]
    cmd+=['-c','init','-c','reset run']
    with tempfile.TemporaryFile() as log:
        proc=subprocess.Popen(cmd,stdout=log,stderr=log);t=None;r=None
        try:
            for _ in range(50):
                if proc.poll() is not None:log.seek(0);raise RuntimeError(log.read().decode())
                try:t=Tcl(a.port);break
                except OSError:time.sleep(.1)
            if t is None:raise RuntimeError('OpenOCD startup timeout')
            time.sleep(.3);r=Relay(t,addr)
            # VER is also the existing maintenance synchronization barrier.
            print('nRF:',r.exchange('VER'),flush=True)
            cap=r.exchange('OTA CAP');print(cap,flush=True)
            if cap!='OTA UART1 BLOCK64 CRC32':raise RuntimeError('Unsupported UART OTA protocol')
            if a.command:print('nRF:',r.exchange(a.command),flush=True);return
            if image is None:return
            crc=binascii.crc32(image)&0xffffffff
            if r.exchange(f'OTA BEGIN {len(image):08X} {crc:08X}')!='READY':raise RuntimeError('BEGIN rejected')
            for offset in range(0,len(image),64):
                command=f'OTA DATA {offset:08X} '+image[offset:offset+64].hex().upper()
                response=r.exchange(command)
                if response!='OK':raise RuntimeError(f'Offset {offset}: {response}')
                if offset%4096==0:print(f'Upload {offset}/{len(image)}',flush=True)
            if r.exchange('OTA COMMIT')!='REBOOT':raise RuntimeError('COMMIT rejected')
            print('Image verified by nRF; waiting for reboot...',flush=True)
            end=time.monotonic()+60
            while time.monotonic()<end:
                time.sleep(1)
                try:
                    version=r.exchange('VER')
                    if f'build={build}' in version and r.exchange('OTA INFO')=='OTA BOOT':
                        print('SUCCESS:',version,'boot state confirmed',flush=True);return
                except RuntimeError:pass
            raise RuntimeError('Update accepted but new image boot not confirmed; keep power and inspect via SWD')
        finally:
            if r:
                try:r.reset()
                except Exception:pass
            if t:
                try:t.cmd('shutdown')
                except Exception:pass
                t.sock.close()
            try:proc.wait(timeout=3)
            except subprocess.TimeoutExpired:proc.terminate();proc.wait(timeout=3)

if __name__=='__main__':
    try:main()
    except Exception as e:raise SystemExit(f'ERROR: {e}')
