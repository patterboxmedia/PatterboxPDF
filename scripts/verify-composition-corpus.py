"""Independent, exact-byte decoded predicates for the authored four-page corpus.

Expected values are authored here, not read from the producer manifest. Resource
names/object numbers are resolved before the metamorphic semantic comparison.
This is a finite corpus inspector, not a renderer or arbitrary PDF validator.
"""
import hashlib
import argparse
import io
import json
from pathlib import Path
from collections import Counter
import xml.etree.ElementTree as ET

from pypdf import PdfReader
from pypdf.generic import (ArrayObject, BooleanObject, ByteStringObject, ContentStream,
                          DictionaryObject, IndirectObject, NameObject, NullObject,
                          NumberObject, FloatObject, StreamObject, TextStringObject)

BLENDS = ['Normal', 'Multiply', 'Screen', 'Overlay', 'Darken', 'Lighten',
          'ColorDodge', 'ColorBurn', 'HardLight', 'SoftLight', 'Difference',
          'Exclusion', 'Hue', 'Saturation', 'Color', 'Luminosity']
PLANES = {'/DeviceGray': bytes([0, 85, 170, 255]),
          '/DeviceRGB': bytes([255, 0, 32, 0, 255, 64, 16, 32, 255, 255, 128, 0]),
          '/DeviceCMYK': bytes([255, 0, 32, 0, 0, 255, 64, 0, 16, 32, 255, 0, 0, 0, 0, 255])}
ALPHA = bytes([0, 64, 128, 255])
TITLE = 'Composition (\\) <&>\rUnicode é 🙂'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def obj(value):
    return value.get_object() if isinstance(value, IndirectObject) else value


def numbers(values):
    return [float(value) for value in values]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def close(actual, expected):
    require(len(actual) == len(expected) and all(abs(float(a)-b) <= 0.000001 for a, b in zip(actual, expected)),
            'numeric operands differ: '+repr((actual, expected)))


class Decoder:
    def __init__(self, reader):
        self.reader = reader

    def operations(self, stream):
        return ContentStream(stream, self.reader).operations

    def normalized(self, value):
        value = obj(value)
        if isinstance(value, StreamObject):
            dictionary = {str(key): self.normalized(child) for key, child in sorted(value.items())
                          if key not in ('/Length', '/Filter', '/DecodeParms', '/Resources')}
            if value.get('/Subtype') == '/Form':
                dictionary['content'] = self.logical(self.operations(value), obj(value.get('/Resources', {})))
            else:
                dictionary['decoded_bytes_sha256'] = sha(value.get_data())
                dictionary['decoded_bytes_count'] = len(value.get_data())
            return dictionary
        if isinstance(value, DictionaryObject) or isinstance(value, dict):
            return {str(key): self.normalized(child) for key, child in sorted(value.items())}
        if isinstance(value, (ArrayObject, list, tuple)):
            return [self.normalized(child) for child in value]
        if isinstance(value, BooleanObject):
            return value.value
        if isinstance(value, NullObject):
            return None
        if isinstance(value, (NumberObject, FloatObject)):
            return float(value)
        if isinstance(value, ByteStringObject):
            return {'bytes_hex': bytes(value).hex()}
        if isinstance(value, (TextStringObject, NameObject, str)):
            return str(value)
        if isinstance(value, (int, float, bool)) or value is None:
            return value
        raise TypeError(type(value))

    def logical(self, operations, resources):
        result = []
        categories = {b'gs': '/ExtGState', b'Do': '/XObject', b'sh': '/Shading'}
        for operands, operator in operations:
            if operator in categories:
                resource = obj(resources[categories[operator]])[operands[0]]
                result.append([operator.decode('ascii'), self.normalized(resource)])
            else:
                result.append([operator.decode('ascii'), self.normalized(operands)])
        return result

    def blocks(self, page_or_form):
        stream = page_or_form if isinstance(page_or_form, StreamObject) else page_or_form['/Contents']
        result, block, depth = [], [], 0
        for operands, operator in self.operations(stream):
            if operator == b'q':
                depth += 1
                if depth == 1:
                    block = []
                    continue
            if operator == b'Q':
                depth -= 1
                require(depth >= 0, 'unbalanced q/Q')
                if depth == 0:
                    result.append(block)
                    continue
            require(depth > 0, 'expected authored item save/restore envelope')
            block.append((operands, operator))
        require(depth == 0, 'unclosed graphics state')
        return result


def states(block, resources):
    state = {}
    for operands, operator in block:
        if operator == b'gs':
            state.update(obj(obj(resources['/ExtGState'])[operands[0]]))
    return state


def single(block, operator):
    values = [operands for operands, actual in block if actual == operator]
    require(len(values) == 1, 'expected exactly one '+str(operator))
    return values[0]


def rectangle(x1, y1, x2, y2):
    return [(b'm', [x1, y1]), (b'l', [x2, y1]), (b'l', [x2, y2]),
            (b'l', [x1, y2]), (b'h', [])]


def path_values(block, expected):
    def expanded(commands):
        result = []
        for operator, operands in commands:
            if operator == b're':
                x, y, width, height = numbers(operands)
                result.extend(rectangle(x, y, x+width, y+height))
            else:
                result.append((operator, operands))
        return result
    actual = expanded([(operator, operands) for operands, operator in block if operator in (b'm', b'l', b'c', b'h', b're')])
    expected = expanded(expected)
    require(len(actual) == len(expected), 'path coordinate count')
    for (operator, operands), (expected_operator, expected_operands) in zip(actual, expected):
        require(operator == expected_operator, 'path command order')
        close(operands, expected_operands)


def all_objects(reader):
    seen = set()
    for generation, entries in reader.xref.items():
        for number in entries:
            if number and (number, generation) not in seen:
                seen.add((number, generation))
                yield (number, generation), reader.get_object(IndirectObject(number, generation, reader))


def function_value(function, x):
    function = obj(function)
    kind = int(function['/FunctionType'])
    close(function['/Domain'], [0, 1])
    if kind == 2:
        require(float(function['/N']) == 1, 'expected authored linear function')
        return [float(a)+(float(b)-float(a))*x for a, b in zip(function['/C0'], function['/C1'])]
    require(kind == 3, 'unsupported function type in authored gradient')
    functions = function['/Functions']
    bounds = [0.0, *numbers(function['/Bounds']), 1.0]
    encode = numbers(function['/Encode'])
    require(len(functions)+1 == len(bounds) and len(encode) == 2*len(functions), 'stitching shape')
    for i in range(len(functions)):
        if x < bounds[i+1] or i == len(functions)-1:
            local = (x-bounds[i])/(bounds[i+1]-bounds[i])
            return function_value(functions[i], encode[2*i]+local*(encode[2*i+1]-encode[2*i]))
    raise ValueError('function sample is outside domain')


def inspect(path, mode, profile_bytes):
    data = path.read_bytes()
    reader = PdfReader(io.BytesIO(data), strict=True)
    decoder = Decoder(reader)
    result = {'schema': 'patterbox-composition-measurements/v1', 'passed': False,
              'input_sha256': sha(data), 'findings': [], 'measurements': {}}
    m = result['measurements']
    try:
        pdfx = mode == 'pdfx4'
        color_space = '/DeviceCMYK' if pdfx else '/DeviceRGB'
        spaces = ['/DeviceGray', '/DeviceCMYK'] + ([] if pdfx else ['/DeviceRGB'])
        require(len(reader.pages) == 4, 'four authored pages required')
        logical_pages = []
        pages = list(reader.pages)
        for page in pages:
            close(page['/MediaBox'], [-10, -10, 610, 610])
            close(page['/BleedBox'], [-5, -5, 605, 605])
            close(page['/TrimBox'], [0, 0, 600, 600])
            logical_pages.append({'boxes': [numbers(page[key]) for key in ('/MediaBox', '/BleedBox', '/TrimBox')],
                                  'group': decoder.normalized(page['/Group']),
                                  'operations': decoder.logical(decoder.operations(page['/Contents']), obj(page['/Resources']))})
        m['page_boxes'] = logical_pages[0]['boxes']
        m['item_counts_by_page'] = [len(decoder.blocks(page)) for page in pages]
        require(m['item_counts_by_page'] == [13, 5*len(spaces), 6, 16], 'authored item count')
        path_page = pages[0]
        blocks = decoder.blocks(path_page)
        counter = Counter(operator.decode('ascii') for block in blocks for _, operator in block)
        m['path_operator_counts'] = dict(sorted(counter.items()))
        require(counter['f'] == 1 and counter['f*'] == 1 and counter['B'] == 1 and counter['B*'] == 1
                and counter['S'] == 9 and counter['c'] == 9 and counter['m'] == 17
                and counter['h'] == 8 and counter['l'] == 42, 'path command or paint partition missing')
        for i, block in enumerate(blocks[:4]):
            close(single(block, b'cm'), [1, 0, 0.125, 1, 30 if i < 2 else 230, 440 if i % 2 == 0 else 320])
            require(states(block, obj(path_page['/Resources']))['/BM'] == '/Normal', 'path blend')
            path_values(block, rectangle(0, 0, 140, 90)+rectangle(35, 20, 105, 70))
            close(single(block, b'k' if pdfx else b'rg'), [1, 0, 0, 0] if pdfx else [1, 0, 0])
        stroke_rows = []
        for index, block in enumerate(blocks[4:]):
            column, row = divmod(index, 3)
            actual = {'cap': int(single(block, b'J')[0]), 'join': int(single(block, b'j')[0]),
                      'width': float(single(block, b'w')[0]), 'miter': float(single(block, b'M')[0]),
                      'dash': numbers(single(block, b'd')[0]), 'phase': float(single(block, b'd')[1]),
                      'transform': numbers(single(block, b'cm'))}
            require(actual == {'cap': column, 'join': row, 'width': [0, 2, 6][row], 'miter': [1, 5, 9][row],
                               'dash': [[], [0, 4], [3, 1, 2]][row], 'phase': column*1.25,
                               'transform': [1, 0, 0, 1, 25+column*185, 35+row*85]}, 'stroke cross product')
            stroke_rows.append(actual)
            path_values(block, [(b'm', [0, 0]), (b'l', [40, 45]),
                                (b'c', [65, 70, 90, -20, 115, 30]), (b'l', [140, 0])])
            close(single(block, b'G'), [0.15])
        m['stroke_cross_product'] = stroke_rows
        objects = list(all_objects(reader))
        images = [(key, value) for key, value in objects if isinstance(value, StreamObject) and value.get('/Subtype') == '/Image']
        image_rows = []
        for key, image in images:
            require(int(image['/Width']) == 2 and int(image['/Height']) == 2 and int(image['/BitsPerComponent']) == 8,
                    'image dimensions/precision')
            samples = image.get_data()
            space = str(image['/ColorSpace'])
            mask = obj(image.get('/SMask'))
            require(samples == ALPHA or samples == PLANES[space], 'unknown image plane')
            require(mask is None or mask.get_data() == ALPHA, 'wrong straight alpha plane')
            image_rows.append({'object': list(key), 'space': space, 'plane_sha256': sha(samples),
                               'plane_hex': samples.hex(), 'alpha_plane_sha256': sha(mask.get_data()) if mask else None})
        counts = Counter((row['space'], row['plane_hex'], row['alpha_plane_sha256']) for row in image_rows)
        expected_counts = Counter()
        for space in spaces:
            expected_counts[(space, PLANES[space].hex(), None)] += 2
            expected_counts[(space, PLANES[space].hex(), sha(ALPHA))] += 2
            expected_counts[('/DeviceGray', ALPHA.hex(), None)] += 2
        require(counts == expected_counts, 'registered image/alpha inventory differs')
        m['all_registered_image_objects'] = image_rows
        image_page = pages[1]
        resources = obj(image_page['/Resources'])
        direct_images = obj(resources['/XObject'])
        require(len(direct_images) == 2*len(spaces), 'unused image acquired a direct content reference')
        image_blocks = decoder.blocks(image_page)
        placement_rows = []
        for column, space in enumerate(spaces):
            four = image_blocks[column*5:column*5+4]
            for row in range(2):
                names = [single(block, b'Do')[0] for block in four[row*2:row*2+2]]
                require(names[0] == names[1], 'shared image was duplicated per placement')
                image = obj(direct_images[names[0]])
                require(image['/ColorSpace'] == space and image.get_data() == PLANES[space], 'placement plane binding')
                require(('/SMask' in image) == (row == 1), 'opaque/alpha placement binding')
                for copy, block in enumerate(four[row*2:row*2+2]):
                    close(single(block, b'cm'), [55, 4, 7, 55, 25+column*185+copy*75, 420-row*150])
                    require(sum(operator == b'W*' for _, operator in block) == 1, 'image clip rule')
                    path_values(block, rectangle(0, 0, 0.875, 1))
                    state = states(block, resources)
                    close([state['/ca'], state['/CA']], [1 if copy == 0 else 0.5]*2)
                    placement_rows.append({'space': space, 'alpha': row == 1, 'copy': copy,
                                           'plane_sha256': sha(image.get_data()), 'opacity': float(state['/ca'])})
            block = image_blocks[column*5+4]
            close(single(block, b'cm'), [90, 0, 0, 90, 30+column*185, 80])
            require(sum(operator == b'W' for _, operator in block) == 1, 'ImageAlpha clip rule')
            path_values(block, rectangle(0, 0, 1, 0.875)+[(b're', [0, 0, 1, 1])])
            close(single(block, b'g'), [0.25])
            state = states(block, resources)
            close([state['/ca'], state['/CA']], [0.75, 0.75])
            mask = obj(state['/SMask'])
            require(mask['/S'] == '/Luminosity', 'ImageAlpha mask type')
            form = obj(mask['/G'])
            form_ops = decoder.operations(form)
            target = obj(obj(form['/Resources'])['/XObject'])[single(form_ops, b'Do')[0]]
            require(obj(target).get_data() == ALPHA, 'ImageAlpha mask source identity')
        m['image_placements'] = placement_rows
        m['image_alpha_placements'] = len(spaces)
        gradient_page = pages[2]
        resources = obj(gradient_page['/Resources'])
        gradient_rows = []
        points = [0, 0.125, 0.25, 0.499999, 0.5, 0.500001, 0.75, 1]
        colors = [[1, 0, 0], [0, 1, 0], [0, 0, 1]] if not pdfx else [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0]]
        for index, block in enumerate(decoder.blocks(gradient_page)):
            radial, style = divmod(index, 3)
            close(single(block, b'cm'), [1.25, 0.125, 0.25, 0.875, 30+radial*280, 50+style*165])
            require(sum(operator == b'W' for _, operator in block) == 1 and sum(operator == b'W*' for _, operator in block) == 1,
                    'gradient item/fill clips')
            path_values(block, rectangle(5, 5, 145, 95)+rectangle(0, 0, 150, 100))
            shading = obj(obj(resources['/Shading'])[single(block, b'sh')[0]])
            require(int(shading['/ShadingType']) == (3 if radial else 2) and shading['/ColorSpace'] == color_space,
                    'gradient type/colorspace')
            coords = [60, 50, 0, 80, 50, 60] if radial else [20, 50, 120, 50]
            close(shading['/Coords'], coords)
            require([value.value for value in shading['/Extend']] == [True, True], 'gradient endpoint padding')
            function = obj(shading['/Function'])
            bounds = [0.5] if style == 2 else [0.25]
            close(function['/Bounds'], bounds)
            require(len(function['/Functions']) == 2, 'gradient function partition')
            samples = [function_value(function, point) for point in points]
            for x, actual in zip(points, samples):
                if style == 2:
                    expected = colors[0] if x < 0.5 else colors[2]
                elif x < 0.25:
                    t = x/0.25
                    expected = [(1-t)*a+t*b for a, b in zip(colors[0], colors[1])]
                else:
                    t = (x-0.25)/0.75
                    expected = [(1-t)*a+t*b for a, b in zip(colors[1], colors[2])]
                close(actual, expected)
            state = states(block, resources)
            alpha_samples = None
            if style == 1:
                close([state['/ca'], state['/CA']], [1, 1])
                mask = obj(state['/SMask'])
                require(mask['/S'] == '/Luminosity', 'gradient alpha mask type')
                form = obj(mask['/G'])
                close(form['/BBox'], [0, 0, 150, 100])
                require(obj(form['/Group'])['/CS'] == '/DeviceGray', 'gradient alpha group space')
                mask_ops = decoder.operations(form)
                require(len(mask_ops) == 1 and mask_ops[0][1] == b'sh', 'gradient mask content shape')
                alpha_shading = obj(obj(obj(form['/Resources'])['/Shading'])[mask_ops[0][0][0]])
                close(alpha_shading['/Coords'], coords)
                require(alpha_shading['/ShadingType'] == shading['/ShadingType'] and alpha_shading['/ColorSpace'] == '/DeviceGray',
                        'gradient color/alpha geometry binding')
                alpha_samples = [function_value(alpha_shading['/Function'], point)[0] for point in points]
                close(alpha_samples, [x*2 if x < 0.25 else 0.5+(x-0.25)*2/3 for x in points])
            else:
                close([state['/ca'], state['/CA']], [0.75 if style == 2 else 0.5]*2)
                require('/SMask' not in state, 'constant alpha acquired a varying-alpha mask')
            gradient_rows.append({'geometry': 'radial' if radial else 'axial', 'style': style, 'coords': numbers(shading['/Coords']),
                                  'function_bounds': numbers(function['/Bounds']), 'sample_points': points,
                                  'color_samples': samples, 'alpha_samples': alpha_samples,
                                  'placement_alpha': float(state['/ca'])})
        m['gradient_measurements'] = gradient_rows
        blend_page = pages[3]
        resources = obj(blend_page['/Resources'])
        blend_rows = []
        for index, block in enumerate(decoder.blocks(blend_page)):
            state = states(block, resources)
            require(state['/BM'] == '/'+BLENDS[index], 'outer blend order')
            close([state['/ca'], state['/CA']], [0.75, 0.75])
            column, row = index % 4, index//4
            close(single(block, b'cm'), [1, 0, 0, 1, 20+column*145, 25+row*140])
            require(sum(operator == b'W*' for _, operator in block) == 1, 'outer group clip')
            path_values(block, rectangle(0, 0, 112, 98))
            outer = obj(obj(resources['/XObject'])[single(block, b'Do')[0]])
            close(outer['/BBox'], [0, 0, 115, 100])
            group = obj(outer['/Group'])
            require(group['/S'] == '/Transparency' and group['/I'].value is True and group['/K'].value is False
                    and group['/CS'] == color_space, 'outer isolated group')
            children = decoder.blocks(outer)
            require(len(children) == 3, 'outer children count')
            child_resources = obj(outer['/Resources'])
            child_states = [states(child, child_resources) for child in children]
            require([state['/BM'] for state in child_states] == ['/Normal', '/Multiply', '/Normal'], 'child blend isolation')
            close([state['/ca'] for state in child_states], [1, 0.5, 0.5])
            path_values(children[0], rectangle(0, 0, 115, 100))
            close(single(children[0], b'g'), [0.8])
            require(sum(operator == b'f*' for _, operator in children[0]) == 1, 'outer backdrop fill rule')
            path_values(children[2], rectangle(55, 35, 110, 95))
            close(single(children[2], b'k' if pdfx else b'rg'), [0, 1, 0, 0] if pdfx else [0, 1, 0])
            close(single(children[1], b'cm'), [1, 0, 0, 1, 10, 10])
            inner = obj(obj(child_resources['/XObject'])[single(children[1], b'Do')[0]])
            close(inner['/BBox'], [0, 0, 90, 60])
            require(obj(inner['/Group'])['/I'].value is True and obj(inner['/Group'])['/CS'] == color_space, 'inner isolated group')
            grandchildren = decoder.blocks(inner)
            require(len(grandchildren) == 2, 'inner child count')
            for child_index, child in enumerate(grandchildren):
                state = states(child, obj(inner['/Resources']))
                require(state['/BM'] == '/Normal', 'grandchild blend isolation')
                close([state['/ca'], state['/CA']], [1, 1])
                path_values(child, rectangle(0 if child_index == 0 else 25, 0, 65 if child_index == 0 else 90, 60))
                expected_color = ([1, 0, 0, 0] if child_index == 0 else [0, 0, 1, 0]) if pdfx else ([1, 0, 0] if child_index == 0 else [0, 0, 1])
                close(single(child, b'k' if pdfx else b'rg'), expected_color)
            blend_rows.append({'blend': BLENDS[index], 'outer_alpha': 0.75, 'inner_alpha': 0.5,
                               'outer_children': 3, 'inner_children': 2, 'isolated': True,
                               'child_blends': [str(state['/BM']) for state in child_states]})
        m['blend_groups'] = blend_rows
        require(reader.metadata.title == TITLE, 'logical Info title')
        require(reader.metadata['/CreationDate'] == 'D:20161231235959Z'
                and reader.metadata['/ModDate'] == 'D:20161231235959Z', 'normalized Info dates')
        root = reader.trailer['/Root']
        packet = root['/Metadata'].get_data()
        xml = ET.fromstring(packet)
        ns = {'rdf': 'http://www.w3.org/1999/02/22-rdf-syntax-ns#', 'dc': 'http://purl.org/dc/elements/1.1/',
              'xmp': 'http://ns.adobe.com/xap/1.0/'}
        require(xml.find('.//dc:title/rdf:Alt/rdf:li', ns).text == TITLE, 'logical XMP title')
        for name in ('CreateDate', 'ModifyDate', 'MetadataDate'):
            require(xml.find('.//xmp:'+name, ns).text == '2016-12-31T23:59:59Z', 'normalized XMP date '+name)
        if pdfx:
            intents = root['/OutputIntents']
            require(len(intents) == 1 and intents[0].get_object()['/DestOutputProfile'].get_data() == profile_bytes,
                    'exact supplied profile identity')
        else:
            require('/OutputIntents' not in root, 'ordinary output has print intent')
        m['metadata'] = {'title': TITLE, 'info_date': reader.metadata['/CreationDate'],
                         'xmp_date': '2016-12-31T23:59:59Z', 'packet_sha256': sha(packet),
                         'output_profile_sha256': sha(profile_bytes) if pdfx else None}
        inventory = sorted([decoder.normalized(image) for _, image in images], key=lambda value: json.dumps(value, sort_keys=True))
        canonical = {'pages': logical_pages, 'all_registered_images': inventory, 'title': TITLE,
                     'date': '2016-12-31T23:59:59Z', 'output_profile': sha(profile_bytes) if pdfx else None}
        result['decoded_semantics_sha256'] = sha(json.dumps(canonical, sort_keys=True, separators=(',', ':')).encode())
        result['passed'] = True
    except Exception as error:
        result['findings'].append(str(error))
    require(path.read_bytes() == data, 'inspector mutated PDF')
    result['unchanged'] = True
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pdf',type=Path,required=True)
    parser.add_argument('--mode',choices=('ordinary','pdfx4'),required=True)
    parser.add_argument('--profile',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    if args.mode=='pdfx4' and args.profile is None:parser.error('pdfx4 requires --profile')
    evaluator_sha=sha(Path(__file__).read_bytes())
    profile=args.profile.read_bytes() if args.profile else b''
    report=inspect(args.pdf,args.mode,profile)
    report.update(input_bytes=args.pdf.stat().st_size,evaluator_sha256=evaluator_sha)
    unchanged=(sha(Path(__file__).read_bytes())==evaluator_sha
               and (args.profile is None or args.profile.read_bytes()==profile))
    report['unchanged'] &= unchanged
    if not unchanged:report['findings'].append('Evaluator or supplied profile changed during inspection')
    report['passed'] &= unchanged
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    return int(not report['passed'])


if __name__=='__main__':raise SystemExit(main())
