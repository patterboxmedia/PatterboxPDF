#!/usr/bin/env python3
"""Independent exact-byte ICC checks for development qualification.

This decoder does not call the native admission helper or a colour engine.
Registration and original measurement provenance require separate evidence.
For 4.2 localized strings, actual Unicode encoding and validity are named
resource preconditions; its directory has no encoding discriminator. The
older editions' required UTF-16BE encoding is checked here.
Optional characterization interchange files and retained PostScript operands
must be valid in their declared languages. This decoder checks their ICC
framing and roles; it does not interpret those nested languages.
"""
import argparse
import hashlib
import json
import math
import struct
from pathlib import Path


def _need(condition, message):
    if not condition:
        raise ValueError(message)


def _u(data, at, size=4):
    _need(0 <= at <= len(data)-size, 'Truncated ICC integer')
    return int.from_bytes(data[at:at+size], 'big')


def _date(data):
    _need(len(data) == 12, 'Date length')
    year, month, day, hour, minute, second = struct.unpack('>6H', data)
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    days = (31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
    _need(year > 0 and 1 <= month <= 12 and 1 <= day <= days[month-1]
          and hour < 24 and minute < 60 and second < 60, 'Invalid ICC date')


def _ascii(data):
    _need(data and data[-1] == 0 and data.isascii(), 'Invalid NUL-terminated ASCII')


def _fixed(data, zero=False):
    stop = data.find(bytes(1))
    _need(stop >= 0 and data[:stop].isascii() and (not zero or not any(data[stop:])), 'Invalid fixed ASCII')
    return data[:stop]


def _curve(data):
    _need(len(data) >= 12 and not any(data[4:8]), 'Invalid curve header')
    if data[:4] == b'curv':
        _need(len(data) == 12+2*_u(data, 8), 'Curve sample extent')
    elif data[:4] == b'para':
        function = _u(data, 8, 2)
        _need(function <= 4 and data[10:12] == bytes(2), 'Parametric function')
        _need(len(data) == 12+4*(1,3,4,5,7)[function], 'Parametric extent')
        params = struct.unpack('>'+'i'*((len(data)-12)//4), data[12:])
        gamma = params[0]
        if function == 0:
            _need(gamma >= 0, 'Undefined parametric power at zero')
        else:
            scale, offset = params[1:3]
            _need(function > 2 or scale != 0, 'Undefined parametric threshold')
            threshold = -offset/scale if function <= 2 else params[4]/65536
            if threshold <= 1:
                if function <= 2:
                    first = 0 if threshold > 0 else offset*65536
                else:
                    first = scale*max(0,params[4])+offset*65536
                last = (scale+offset)*65536
                low,high = sorted((first,last))
                _need(not (gamma < 0 and low <= 0 <= high), 'Parametric power has an active zero pole')
                _need(gamma%65536 == 0 or low >= 0, 'Parametric power has a negative real base')
                for base in (first,last):
                    try:
                        result = math.pow(base/4294967296,gamma/65536)
                    except (ValueError,OverflowError):
                        raise ValueError('Undefined parametric power') from None
                    _need(math.isfinite(result), 'Nonfinite parametric power')
    else:
        raise ValueError('Invalid curve type')


def inspect_multistage_lut(payload, expected_channels):
    """Decode embedded curves, matrix and CLUT using tag-relative offsets."""
    problems, spans = [], []
    def u32(start):
        _need(start+4 <= len(payload), 'Truncated multistage integer')
        return int.from_bytes(payload[start:start+4], 'big')
    def span(start, size, kind):
        end = start+size
        _need(start >= 32 and start%4 == 0 and end <= len(payload), 'Multistage element bounds')
        spans.append((start,end,kind))
        return end
    def curves(start, count):
        for _ in range(count):
            _need(start+12 <= len(payload) and not any(payload[start+4:start+8]), 'Invalid embedded curve header')
            kind = payload[start:start+4]
            if kind == b'curv':
                size = 12+2*u32(start+8)
            elif kind == b'para':
                function = int.from_bytes(payload[start+8:start+10], 'big')
                _need(function <= 4 and payload[start+10:start+12] == bytes(2), 'Invalid parametric curve function')
                size = 12+4*(1,3,4,5,7)[function]
            else:
                raise ValueError('Invalid embedded curve signature')
            _curve(payload[start:start+size])
            end = span(start,size,'curve')
            padded = (end+3)&~3
            _need(not any(payload[end:min(padded,len(payload))]), 'Nonzero embedded curve padding')
            start = padded
    try:
        _need(len(payload) >= 32 and not any(payload[4:8]+payload[10:12]), 'Invalid multistage header')
        inputs, outputs = payload[8:10]
        _need(1 <= inputs <= 15 and 1 <= outputs <= 15 and
                (expected_channels is None or (inputs,outputs) == expected_channels), 'Multistage channel count')
        b, matrix, m, clut, a = (u32(start) for start in (12,16,20,24,28))
        _need(b and bool(a) == bool(clut) and bool(m) == bool(matrix)
                and (inputs == outputs or clut), 'Invalid multistage stage combination')
        forward = payload[:4] == b'mAB '
        side = outputs if forward else inputs
        curves(b,side)
        if a: curves(a,inputs if forward else outputs)
        if m:
            _need(side == 3, 'Matrix stage requires three channels')
            curves(m,side); span(matrix,48,'matrix')
        if clut:
            span(clut,20,'CLUT header')
            grids = payload[clut:clut+16]
            precision = payload[clut+16]
            _need(all(n >= 2 for n in grids[:inputs]) and not any(grids[inputs:])
                    and precision in (1,2) and not any(payload[clut+17:clut+20]), 'Invalid multistage CLUT header')
            end = span(clut,20+math.prod(grids[:inputs])*outputs*precision,'CLUT')
            _need(not any(payload[end:min((end+3)&~3,len(payload))]), 'Nonzero multistage CLUT padding')
            spans = [item for item in spans if item[2] != 'CLUT header']
        unique = sorted(set(spans))
        for left,right in zip(unique,unique[1:]):
            _need(left[1] <= right[0], 'Overlapping multistage elements')
    except (ValueError,IndexError) as error:
        problems.append(str(error))
    return problems



def _description(data):
    count = _u(data,8)
    _need(count > 0 and 12+count <= len(data), 'Description ASCII extent')
    _ascii(data[12:12+count])
    count_u = _u(data,16+count)
    script = 20+count+2*count_u
    _need(script+70 <= len(data), 'Description Unicode/script extent')
    text = data[20+count:script]
    text.decode('utf-16-be')
    _need(not count_u or text.endswith(bytes(2)), 'Description Unicode terminator')
    count_s = data[script+2]
    _need(count_s <= 67 and (not count_s or data[script+2+count_s] == 0), 'Description script extent/terminator')
    return script+70


def _localized(data, utf16_required=True):
    _need(data[:4] == b'mluc' and not any(data[4:8]), 'Localized text type')
    count, stride = _u(data,8),_u(data,12)
    _need((count==0 or stride>=12) and 16+count*stride <= len(data), 'Localized text records')
    table = 16+stride*count
    spans = ([],[])
    extent = table
    for row in range(count):
        at = 16+stride*row
        length, offset = _u(data,at+4),_u(data,at+8)
        end = offset+length
        _need((not utf16_required or length%2 == 0) and offset >= table and end <= len(data), 'Localized string bounds')
        if utf16_required and length:
            first,last=_u(data,offset,2),_u(data,end-2,2)
            _need(not 0xdc00<=first<=0xdfff and not 0xd800<=last<=0xdbff,'Localized UTF-16 endpoint')
        extent=max(extent,end)
        if utf16_required:spans[offset%2].append((offset,end))
    for ranges in spans:
        merged=[]
        for start,end in sorted(ranges):
            if merged and start<=merged[-1][1]:merged[-1]=(merged[-1][0],max(end,merged[-1][1]))
            else:merged.append((start,end))
        for start,end in merged:data[start:end].decode('utf-16-be')
    return extent


def _standard(kind,version):
    common = {b'curv',b'data',b'dtim',b'mft1',b'mft2',b'meas',b'ncl2',b'pseq',b'rcs2',b'sf32',b'sig ',b'text',b'uf32',b'ui16',b'ui32',b'ui64',b'ui08',b'view',b'XYZ '}
    if version[0] == 2:
        return kind in common | {b'desc',b'ncol',b'devs',b'crdi',b'scrn',b'bfd '}
    if version[1] == 0:
        common |= {b'devs',b'crdi',b'scrn',b'bfd '}
    return kind in common | {b'chrm',b'clro',b'clrt',b'mAB ',b'mBA ',b'mluc',b'para'}


def _role(name,p,profile):
    version=profile[8:12]
    v4=version[0]==4
    expected={b'desc':b'mluc' if v4 else b'desc',b'cprt':b'mluc' if v4 else b'text',
              **{n:b'XYZ ' for n in (b'wtpt',b'bkpt',b'rXYZ',b'gXYZ',b'bXYZ',b'lumi')},
              **{n:b'mluc' if v4 else b'desc' for n in (b'dmnd',b'dmdd',b'vued')},
              b'calt':b'dtim',b'targ':b'text',b'meas':b'meas',b'view':b'view',b'tech':b'sig ',
              b'ncl2':b'ncl2',b'resp':b'rcs2',b'pseq':b'pseq'}
    if v4:
        expected.update({b'chad':b'sf32',b'chrm':b'chrm',b'clro':b'clro',b'clrt':b'clrt',b'clot':b'clrt'})
    if not v4 or version[1]==0:
        expected.update({b'crdi':b'crdi',b'devs':b'devs',b'scrd':b'mluc' if v4 else b'desc',b'scrn':b'scrn',b'bfd ':b'bfd ',
                         **{n:b'data' for n in (b'psd0',b'psd1',b'psd2',b'psd3',b'ps2s',b'ps2i')}})
    if not v4:expected[b'ncol']=b'ncol'
    _need(name not in expected or p[:4]==expected[name], 'Incorrect standard tag type')
    if name in (b'wtpt',b'bkpt',b'rXYZ',b'gXYZ',b'bXYZ',b'lumi'):_need(len(p)==20,'Single XYZ role extent')
    if name==b'tech':_need(len(p)==12 and p[8:12] in (bytes(4),b'fscn',b'dcam',b'rscn',b'ijet',b'twax',b'epho',b'esta',b'dsub',b'rpho',b'fprn',b'vidm',b'vidc',b'pjtv',b'CRT ',b'PMD ',b'AMD ',b'KPCD',b'imgs',b'grav',b'offs',b'silk',b'flex'),'Technology value')
    if name==b'targ':
        _need(len(p)>=16 and all(32<=v<=126 for v in p[8:15]),'Characterization identifier')
        if p[8:15]==b'ICCHDAT':_need(len(p)>=18 and p[15]==32 and 0 not in p[16:-1],'Characterization reference framing')
    if name in (b'rTRC',b'gTRC',b'bTRC',b'kTRC'):_need(p[:4] in ({b'curv',b'para'} if v4 else {b'curv'}),'TRC role type')
    if v4 and name==b'chad':
        _need(len(p)==44,'Adaptation count')
        m=struct.unpack('>9i',p[8:])
        _need(m[0]*(m[4]*m[8]-m[5]*m[7])-m[1]*(m[3]*m[8]-m[5]*m[6])+m[2]*(m[3]*m[7]-m[4]*m[6]) != 0,'Singular adaptation')
    channels = ((4,3) if name in (b'A2B0',b'A2B1',b'A2B2') else (3,4) if name in (b'B2A0',b'B2A1',b'B2A2')
                else (3,1) if name==b'gamt' else (3,3) if name in (b'pre0',b'pre1',b'pre2') else None)
    if channels:
        allowed={b'mft1',b'mft2'}
        if v4:allowed.add(b'mAB ' if name.startswith(b'A2B') else b'mBA ')
        _need(p[:4] in allowed and len(p)>=12 and tuple(p[8:10])==channels,'Transform role/type/channels')
        if p[:4] in (b'mft1',b'mft2'):
            _need(len(p)>=48,'LUT header')
            if profile[20:24]!=b'XYZ ' or name.startswith(b'A2B'):
                _need(p[12:48]==struct.pack('>9i',65536,0,0,0,65536,0,0,0,65536),'Non-XYZ transform matrix')


def _body(p,profile):
    _need(len(p)>=8 and not any(p[4:8]),'Tag generic framing')
    kind=p[:4]
    if not _standard(kind,profile[8:12]):return
    if kind==b'XYZ ':
        _need(len(p)>=20 and (len(p)-8)%12==0,'XYZ extent')
        _need(all(v>=0 for (v,) in struct.iter_unpack('>i',p[8:])),'Negative XYZ')
    elif kind==b'text':_ascii(p[8:])
    elif kind==b'desc':_need(_description(p)==len(p),'Description trailing bytes')
    elif kind==b'mluc':_need(_localized(p,profile[9]!=0x20)==len(p),'Localized trailing bytes')
    elif kind in (b'curv',b'para'):_curve(p)
    elif kind in (b'mft1',b'mft2'):
        _need(len(p)>=48,'LUT header')
        inputs,outputs,grid,reserved=p[8:12]
        _need(1<=inputs<=15 and 1<=outputs<=15 and grid>=2 and reserved==0,'LUT dimensions')
        if kind==b'mft1':size=48+256*(inputs+outputs)+grid**inputs*outputs
        else:
            ni,no=_u(p,48,2),_u(p,50,2)
            _need(2<=ni<=4096 and 2<=no<=4096,'LUT16 table count')
            size=52+2*(inputs*ni+outputs*no+grid**inputs*outputs)
        _need(len(p)==size,'LUT sample extent')
    elif kind in (b'mAB ',b'mBA '):
        errors=inspect_multistage_lut(p,None)
        _need(not errors,'; '.join(errors))
    elif kind in (b'sf32',b'uf32',b'ui16',b'ui32',b'ui64'):
        width={b'sf32':4,b'uf32':4,b'ui16':2,b'ui32':4,b'ui64':8}[kind]
        _need((len(p)-8)%width==0,'Array stride')
    elif kind==b'sig ':_need(len(p)==12,'Signature extent')
    elif kind==b'dtim':_date(p[8:])
    elif kind==b'data':
        flag=_u(p,8);_need(flag in (0,1),'Data flag')
        if flag==0:_ascii(p[12:])
    elif kind in (b'meas',b'view'):
        _need(len(p)==36,'Measurement/view extent')
        _need(_u(p,32)<=8,'Illuminant enum')
        xyz=p[12:24] if kind==b'meas' else p[8:32]
        _need(all(v>=0 for (v,) in struct.iter_unpack('>i',xyz)),'Measurement/view negative XYZ')
        if kind==b'meas':_need(_u(p,8)<=2 and _u(p,24)<=2 and _u(p,28)<=65536,'Measurement enumerations')
    elif kind==b'chrm':
        count,phosphor=_u(p,8,2),_u(p,10,2)
        _need(count>0 and len(p)==12+8*count and phosphor<=4,'Chromaticity dimensions')
        if phosphor:
            coords=((.64,.33,.30,.60,.15,.06),(.63,.34,.31,.595,.155,.07),(.64,.33,.29,.60,.15,.06),(.625,.34,.28,.605,.155,.07))[phosphor-1]
            _need(count==3 and tuple(v for(v,)in struct.iter_unpack('>I',p[12:]))==tuple(round(v*65536) for v in coords),'Named phosphor coordinates')
    elif kind==b'clro':
        count=_u(p,8)
        _need(count==4 and len(p)==16 and set(p[12:])==set(range(4)),'Colorant permutation')
    elif kind==b'clrt':
        count=_u(p,8);_need(count==4 and len(p)==12+38*count,'Colorant count')
        names=[_fixed(p[at:at+32],True) for at in range(12,len(p),38)]
        _need(len(set(names))==count,'Duplicate colorant name')
    elif kind==b'ncl2':
        count,channels=_u(p,12),_u(p,16)
        _need(_u(p,8)&65535==0 and channels in (0,4) and len(p)==84+count*(38+2*channels),'Named color extent/flags')
        _fixed(p[20:52]);_fixed(p[52:84])
        for at in range(84,len(p),38+2*channels):_fixed(p[at:at+32])
    elif kind==b'ncol':
        _need(_u(p,8)&65535==0,'Legacy named color flags')
        count=_u(p,12);at=16
        for index in range(count+2):
            end=p.find(bytes(1),at)
            _need(at<=end<at+32 and p[at:end].isascii(),'Legacy color name')
            at=end+1+(4 if index>=2 else 0)
        _need(at==len(p),'Legacy color extent')
    elif kind==b'pseq':
        count=_u(p,8);at=12
        _need(count<=(len(p)-12)//20,'Sequence count')
        for _ in range(count):
            _need(_u(p,at+12)&~(3 if profile[8]==2 else 15)==0,'Sequence attributes')
            at+=20
            for _ in range(2):
                tail=p[at:]
                _need(len(tail)>=8 and not any(tail[4:8]),'Sequence nested header')
                if profile[8]==2:
                    _need(tail[:4]==b'desc','Sequence description type');at+=_description(tail)
                else:at+=_localized(tail,profile[9]!=0x20)
        _need(at==len(p),'Sequence extent')
    elif kind==b'rcs2':_response(p)
    elif kind==b'devs':_settings(p)
    elif kind==b'crdi':
        at=8
        for _ in range(5):
            count=_u(p,at);_need(at+4+count<=len(p),'CRD extent')
            if count:_ascii(p[at+4:at+4+count])
            at+=4+count
        _need(at==len(p),'CRD trailing bytes')
    elif kind==b'scrn':
        count=_u(p,12)
        _need(_u(p,8)&0xfffc==0 and len(p)==16+12*count,'Screen count/flags')
        _need(all(_u(p,at+8)<=7 for at in range(16,len(p),12)),'Screen spot shape')
    elif kind==b'bfd ':
        at=8
        for _ in range(2):at+=4+2*_u(p,at)
        _need(at<=len(p),'UCR/BG extent');_ascii(p[at:])


def _response(p):
    channels,count=_u(p,8,2),_u(p,10,2)
    _need(channels==4 and count>0 and 12+4*count<=len(p),'Response directory')
    spans,units=[],set()
    for index in range(count):
        offset=_u(p,12+4*index)
        _need(offset>=12+4*count and offset%4==0 and offset+68<=len(p),'Response offset')
        unit=p[offset:offset+4]
        _need(unit in (b'StaA',b'StaE',b'StaI',b'StaT',b'StaM',b'DN  ',b'DN P',b'DNN ',b'DNNP') and unit not in units,'Response measurement unit')
        units.add(unit)
        _need(all(v>=0 for(v,)in struct.iter_unpack('>i',p[offset+20:offset+68])),'Response XYZ')
        at=offset+68
        for channel in range(4):
            number=_u(p,offset+4+4*channel)
            stop=at+8*number;_need(stop<=len(p),'Response samples')
            previous=-1
            for code,reserved,value in struct.iter_unpack('>HHi',p[at:stop]):
                _need(code>previous and reserved==0,'Response ordering/reserved')
                previous=code
            at=stop
        spans.append((offset,at))
    spans.sort()
    _need(all(a[1]<=b[0] for a,b in zip(spans,spans[1:])) and spans[-1][1]==len(p),'Response packing')


def _settings(p):
    platforms=_u(p,8);at=12
    _need(platforms<=(len(p)-12)//12,'Settings platform count')
    for _ in range(platforms):
        platform=p[at:at+4];size,combos=_u(p,at+4),_u(p,at+8)
        end=at+size;_need(size>=12 and end<=len(p),'Settings platform bounds')
        cursor=at+12;_need(combos<=(end-cursor)//8,'Settings combinations count')
        for _ in range(combos):
            size,count=_u(p,cursor),_u(p,cursor+4)
            stop=cursor+size;_need(size>=8 and stop<=end and count<=(size-8)//12,'Settings combination bounds')
            setting=cursor+8
            for _ in range(count):
                kind=p[setting:setting+4];width,values=_u(p,setting+4),_u(p,setting+8)
                limit=setting+12+width*values;_need(limit<=stop,'Setting value bounds')
                if platform in (b'MSFT',b'msft'):
                    if kind==b'rsln':_need(width==8,'Resolution width')
                    elif kind in (b'mtyp',b'hftn'):
                        _need(width==4,'Setting word width')
                        for at_value in range(setting+12,limit,4):
                            value=_u(p,at_value)
                            _need(value>=256 or 0<=value<=(3 if kind==b'mtyp' else 10),'Reserved setting value')
                setting=limit
            _need(setting==stop,'Settings combination extent');cursor=stop
        _need(cursor==end,'Settings platform extent');at=end
    _need(at==len(p),'Settings trailing bytes')


def inspect_profile_bytes(data):
    """Return exact-byte findings; external facts are recorded separately."""
    data=bytes(data)
    try:
        _need(132<=len(data)<=16*1024*1024,'Profile byte bound')
        _need(_u(data,0)==len(data),'Exact declared size')
        version=data[8:12]
        _need(version in (bytes.fromhex('02200000'),bytes.fromhex('04000000'),bytes.fromhex('04100000'),bytes.fromhex('04200000')),'Supported edition')
        _need(data[12:20]==b'prtrCMYK' and data[20:24] in (b'XYZ ',b'Lab ') and data[36:40]==b'acsp','Header class/color/PCS/signature')
        _date(data[24:36])
        _need(data[68:80]==bytes.fromhex('0000f6d6000100000000d32d'),'Header D50')
        _need(not any(data[84 if version[0]==2 else 100:128]),'Reserved header bytes')
        _need(_u(data,44)&0xfffc==0 and _u(data,60)&~(3 if version[0]==2 else 15)==0 and (_u(data,64)&65535)<=3 and (version!=bytes.fromhex('04200000') or _u(data,64)<=3),'Header flags/attributes/intent')
        platforms={bytes(4),b'APPL',b'MSFT',b'SGI ',b'SUNW'}
        if version[0]==2 or version[1]<=0x10:platforms.add(b'TGNT')
        _need(data[40:44] in platforms,'Platform signature')
        if version[0]==4 and any(data[84:100]):
            normalized=bytearray(data);normalized[64:68]=bytes(4);normalized[84:100]=bytes(16)
            if version[1]==0:normalized[56:64]=bytes(8)
            else:normalized[44:48]=bytes(4)
            _need(hashlib.md5(normalized).digest()==data[84:100],'Edition profile ID')
        count=_u(data,128);table=132+12*count
        _need(count>0 and table<=len(data),'Tag table extent')
        names,records=set(),[]
        for name,offset,size in struct.iter_unpack('>4sII',data[132:table]):
            _need(name not in names and offset>=table and offset%4==0 and size>=8 and offset+size<=len(data),'Tag directory bounds/uniqueness')
            names.add(name);records.append((offset,offset+size,name))
        required={b'desc',b'cprt',b'wtpt',b'gamt',b'A2B0',b'A2B1',b'A2B2',b'B2A0',b'B2A1',b'B2A2'}
        _need(required<=names,'Required printer roles')
        ranges=sorted({(start,end) for start,end,name in records})
        _need(all(a[1]<=b[0] for a,b in zip(ranges,ranges[1:])),'Partial directory aliases')
        if version[0]==4:
            cursor=table
            for start,end in ranges:
                _need(start==cursor,'v4 contiguous tag packing')
                cursor=(end+3)&~3
                _need(cursor<=len(data) and not any(data[end:cursor]),'v4 NULL tag padding')
            _need(cursor==len(data),'v4 padded profile extent')
        view=memoryview(data)
        for start,end,name in records:_role(name,view[start:end],data)
        if version[0]==4:
            y={name:data[start+12:start+16] for start,end,name in records if name in (b'view',b'lumi')}
            _need(len(y)<2 or y[b'view']==y[b'lumi'],'Viewing illuminance and luminance disagree')
        if version[0]==4 and b'chad' not in names:
            for start,end,name in records:
                if name==b'meas' and end-start==36:_need(_u(data,start+32) not in range(2,9),'Non-D50 profile measurements need adaptation')
        for start,end in ranges:_body(data[start:end],data)
        return []
    except (ValueError,IndexError,struct.error,UnicodeError,OverflowError) as error:
        return [str(error)]


def inspect_profile(data):
    """Expose the actual decoded container values alongside independent checks."""
    data=bytes(data)
    findings=inspect_profile_bytes(data)
    observations={}
    if not findings:
        records=list(struct.iter_unpack('>4sII',data[132:132+12*_u(data,128)]))
        observations={'declared_size':_u(data,0),'version_bytes':list(data[8:12]),
            'device_class':data[12:16].decode('ascii'),'data_color_space':data[16:20].decode('ascii'),
            'pcs':data[20:24].decode('ascii'),'creation_date':list(struct.unpack('>6H',data[24:36])),
            'header_illuminant_fixed':list(struct.unpack('>3i',data[68:80])),
            'tags':[{'signature':tag.decode('latin1'),'offset':start,'bytes':size,
                     'type':data[start:start+4].decode('latin1')} for tag,start,size in records]}
        observations['parametric_curves']=[{'signature':tag.decode('latin1'),
            'function':_u(data,start+8,2),'coefficients_fixed':list(struct.unpack('>'+'i'*((size-12)//4),data[start+12:start+size]))}
            for tag,start,size in records if data[start:start+4]==b'para']
        observations['view_luminance_y_fixed']={tag.decode('latin1'):int.from_bytes(data[start+12:start+16],'big',signed=True)
            for tag,start,size in records if tag in (b'view',b'lumi')}
    digest=hashlib.sha256(data).hexdigest()
    return {'schema':'patterboxpdf-independent-icc/v1','passed':not findings,
        'input_sha256':digest,'profile_sha256':digest,'bytes':len(data),
        'evaluator_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        'observations':observations,'findings':findings}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    evaluator_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    data=args.profile.read_bytes()
    input_sha256=hashlib.sha256(data).hexdigest()
    report=inspect_profile(data)
    findings=report['findings']
    unchanged=(hashlib.sha256(args.profile.read_bytes()).hexdigest()==input_sha256
               and hashlib.sha256(Path(__file__).read_bytes()).hexdigest()==evaluator_sha256)
    if not unchanged:findings.append('Input or evaluator identity changed during inspection')
    report.update(passed=not findings,unchanged=unchanged,
        external_facts='Registration, original measurement provenance and actual Unicode encoding/validity of 4.2 localized strings require separate context evidence.',
        nested_language_conditions='Valid declared characterization interchange files and edition-applicable PostScript operands are resource preconditions; this decoder checks their ICC framing and roles.')
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    return int(bool(findings))


if __name__=='__main__':raise SystemExit(main())
