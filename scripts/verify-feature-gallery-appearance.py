"""Repeat 50 authored appearance checks on the three-page feature gallery.

Consumes hash-bound renders from verify-feature-gallery.py without replacing
its whole-page comparison verdict. This is a fixture verifier, not a PDF/X validator.
"""
import argparse
import hashlib
import json
from pathlib import Path
import numpy as np
from PIL import Image

if not __debug__:
    raise SystemExit('Run this verifier without -O; its scenario assertions must remain enabled.')

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--renders', type=Path, required=True,
                    help='Receipt directory created by verify-feature-gallery.py')
parser.add_argument('--output', type=Path, required=True, help='Fresh JSON receipt file')
args = parser.parse_args()
renders = args.renders
comparison_path = renders / 'results.json'
comparison = json.loads(comparison_path.read_text(encoding='utf-8'))
assert comparison['schema'] == 'pdf-library-feature-gallery-verification/v1'
assert len(comparison['files']) == 2
assert {item['mode'] for item in comparison['files']} == {'ordinary', 'pdfx4'}
render_inputs = []
for item in comparison['files']:
    folder = 'ordinary' if item['mode'] == 'ordinary' else 'print'
    assert [page['page'] for page in item['pages']] == [1, 2, 3]
    for page in item['pages']:
        assert page['size_px'] == [1220, 1300]
        assert {image['name'] for image in page['renders']} == {
            f'pdfium-{page["page"]}.png', f'poppler-{page["page"]}.png'}
        for image in page['renders']:
            relative = folder + '/' + image['name']
            actual = hashlib.sha256((renders / relative).read_bytes()).hexdigest()
            assert actual == image['sha256'], ('Render identity drift', relative)
            render_inputs.append({'path': relative, 'sha256': actual})
records = []


def read_image(path):
    with Image.open(path) as source:
        assert source.size == (1220, 1300), ('Unexpected gallery dimensions', source.size)
        return np.asarray(source.convert('RGB')).copy()


def pixel(image, x, y):
    # Renders are exactly two pixels per PDF point; y increases upward in PDF.
    return image[round((650 - y) * 2), round(x * 2)].astype(int)

def expected(image, x, y, color, label, tolerance=3, page=1):
    actual = pixel(image, x, y)
    assert np.max(np.abs(actual - color)) <= tolerance, (label, actual, color)
    return {'scenario':label,'page':page,'point':[x,y],'actual':actual.tolist(),
            'expected':color,'tolerance':tolerance,'passed':True}

def region(image, bounds):
    x1, y1, x2, y2 = bounds
    return image[round((650-y2)*2):round((650-y1)*2),round(x1*2):round(x2*2)]

def differences(first, second, bounds):
    a, b = region(first, bounds), region(second, bounds)
    delta = np.abs(a.astype(int) - b.astype(int))
    maximum = delta.max(axis=2)
    return {'mean_channel_difference':float(delta.mean()),
            'p95_max_channel_difference':float(np.percentile(maximum,95)),
            'pixels_above_10_percent':float((maximum>10).mean()*100)}

for engine in ('pdfium', 'poppler'):
    images = [read_image(renders/'ordinary'/f'{engine}-{page}.png')
              for page in (1,2,3)]
    probes = [
        expected(images[0], 45, 400, [255,0,0], 'RGB axial start padding'),
        expected(images[0], 125, 400, [0,255,0], 'RGB interior multistop'),
        expected(images[0], 265, 400, [0,0,255], 'RGB axial end padding'),
        expected(images[0], 145, 240, [255,0,0], 'RGB hard stop left limit'),
        expected(images[0], 165, 240, [0,0,255], 'RGB hard stop right limit'),
        expected(images[0], 455, 405, [0,0,255], 'RGB radial center', 5),
        expected(images[0], 455, 250, [0,255,0], 'variable alpha radial opaque center', 5),
        expected(images[0], 345, 250, [179,179,179], 'variable alpha radial transparent padding'),
        expected(images[0], 355, 90, [255,255,255], 'axial transparent endpoint'),
        expected(images[0], 455, 90, [102,255,102], 'independent color and alpha midpoint', 5),
        expected(images[0], 75, 90, [255,128,128], 'isolated group half opacity'),
        expected(images[0], 155, 90, [128,128,255], 'group opacity once at inner overlap'),
        expected(images[0], 255, 90, [128,128,255], 'group overlap and nonoverlap agree'),
        expected(images[1], 145, 200, [0,0,0], 'Gray hard stop left',page=2),
        expected(images[1], 165, 200, [255,255,255], 'Gray hard stop right',page=2),
        expected(images[1], 150, 120, [255,255,255], 'transform clip excludes outside point',page=2),
        expected(images[2], 70, 530, [64,0,191], 'Normal blend on completed group',page=3),
        expected(images[2], 215, 530, [64,0,0], 'Multiply blend on completed group',page=3),
        expected(images[2], 360, 530, [255,0,191], 'Screen blend on completed group',page=3),
    ]
    inside = pixel(images[1], 140, 65)
    assert inside.min() < 240 and inside.max() - inside.min() <= 2
    probes.append({'scenario':'transformed triangular clip retains gray alpha paint',
                   'page':2,'point':[140,65],'actual':inside.tolist(),
                   'predicate':'minimum channel < 240 and channel spread <= 2','passed':True})
    records.append({'engine':engine,'mode':'ordinary','probes':probes})
    print_images = [read_image(renders/'print'/f'{engine}-{page}.png')
                    for page in (1,2,3)]
    # The endpoint colors agree with opaque CMYK image pixels in each reader.
    # Color interpolation still differs between readers; do not relabel it a pass.
    cyan = pixel(print_images[0], 410, 595)
    cyan_left = pixel(print_images[0], 45, 240)
    yellow_right = pixel(print_images[0], 265, 240)
    assert np.max(np.abs(cyan_left - cyan)) <= 3
    assert np.max(np.abs(pixel(print_images[0],145,240)-cyan_left)) <= 3
    assert np.max(np.abs(pixel(print_images[0],165,240)-yellow_right)) <= 3
    assert np.max(np.abs(cyan_left-yellow_right)) > 100
    records.append({'engine':engine,'mode':'pdfx4','probes':[
        {'scenario':'CMYK hard transition retains distinct flat limits',
         'page':1,'padding_points':[[45,240],[265,240]],
         'limit_points':[[145,240],[165,240]],
         'predicate':'each limit agrees with its padding color within 3; padding colors differ by more than 100 in at least one channel',
         'left':cyan_left.tolist(),'right':yellow_right.tolist(),'passed':True},
        {'scenario':'CMYK gradient padding agrees with same-reader decoded image cyan',
         'page':1,'gradient_point':[45,240],'image_point':[410,595],
         'gradient':cyan_left.tolist(),'image':cyan.tolist(),'tolerance':3,'passed':True},
        expected(print_images[1],150,120,[255,255,255],'print transformed clip excludes outside point',page=2),
        expected(print_images[1],145,200,[0,0,0],'print Gray hard stop left',page=2),
        expected(print_images[1],165,200,[255,255,255],'print Gray hard stop right',page=2),
    ]})

regions = {'gray_axial':[40,485,270,585], 'gray_radial':[340,485,570,585],
           'cmyk_axial':[35,315,275,425], 'cmyk_radial':[335,315,575,425],
           'gray_hardstop':[40,160,270,260], 'cmyk_hardstop':[335,155,575,265],
           'gray_transform_alpha_clip':[35,30,280,125],
           'cmyk_transform_alpha_clip':[335,25,580,135]}
first = read_image(renders/'ordinary/pdfium-2.png')
second = read_image(renders/'ordinary/poppler-2.png')
local = {name: differences(first,second,bounds) for name,bounds in regions.items()}
whole = differences(first,second,[0,0,610,650])
# Drop only CMYK probe rectangles for localization, without changing the published
# whole-page comparison or using the filtered result as a replacement pass gate.
mask = np.ones(first.shape[:2],dtype=bool)
for name,bounds in regions.items():
    if name.startswith('cmyk_'):
        x1,y1,x2,y2=bounds
        mask[round((650-y2)*2):round((650-y1)*2),round(x1*2):round(x2*2)]=False
delta=np.abs(first.astype(int)-second.astype(int)).max(axis=2)
outside_high=int(np.count_nonzero((delta>10)&mask))
inside_high=int(np.count_nonzero((delta>10)&~mask))
record={'schema':'pdf-library-u5-appearance-probes/v1', 'all_scenario_probes_passed':True,
    'scenario_probes':records, 'page2_region_metrics':local,
    'page2_whole_metrics':whole,'page2_high_difference_pixels':{
        'inside_cmyk_probe_rectangles':inside_high,'outside_cmyk_probe_rectangles':outside_high},
    'strict_gallery_comparison_passed':comparison['passed'],
    'input_pdfs':[{'file':item['file'],'sha256':item['sha256'],'mode':item['mode']} for item in comparison['files']],
    'input_renders':render_inputs,
    'probe_runner_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    'strict_comparison_receipt_sha256':hashlib.sha256((renders/'results.json').read_bytes()).hexdigest(),
    'probe_runner_path':'scripts/verify-feature-gallery-appearance.py',
    'normative_pdfx4_coverage_complete':False,
    'interpretation':'Authored scenario probes pass independently in both readers; the whole-page comparison result and metrics remain recorded separately.'}
with args.output.open('x', encoding='utf-8', newline='\n') as output:
    output.write(json.dumps(record,indent=2)+'\n')
print(json.dumps({'scenario_probes':sum(len(r['probes']) for r in records),
                  'all_scenario_probes_passed':True,
                  'strict_gallery_comparison_passed':comparison['passed']}))

