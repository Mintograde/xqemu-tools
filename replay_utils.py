import base64
import datetime
import glob
import io
import json
import os
from pprint import pprint

import simdjson

import zstandard as zstd

from database import DBConnector


necessary_fields = dict(
    summary={},
    game_meta={},
    events={},
    spawns=dict(
        spawn_id={},
        x={},
        y={},
        z={},
        facing={},
        team_index={},
        gametypes={},
    ),
    items={},
    ticks=dict(
        game_type={},
        variant={},
        game_engine_has_teams={},
        multiplayer_map_name={},
        game_time_info=dict(
            game_time={},
            real_time_elapsed={},
        ),
        damage_counts={},
        current_time={},
        start_time={},
        game_id={},
        performance={},
        players=dict(
            player_index={},
            local_player={},
            name={},
            team={},
            respawn_timer={},
            camo_timer={},
            kill_streak={},
            multikill={},
            time_of_last_kill={},
            kills={},
            assists={},
            team_kills={},
            deaths={},
            suicides={},
            score={},
            ctf_score={},
            observer_camera_info=dict(
                x={},
                y={},
                z={},
                x_aim={},
                y_aim={},
                z_aim={},
                fov={},

            ),
            player_object_data={},
            model_nodes={},
            derived_stats={},
            input_data={},
        ),
        objects=dict(
            object_id={},
            x={},
            y={},
            z={},
            object_type_string={},
        ),
        object_meta={},
    )
)
necessary_fields_dotted = [
    'summary.*',
    'game_meta.*',
    'events.*',
    'spawns.spawn_id',
    'spawns.x',
    'spawns.y',
    'spawns.z',
    'spawns.facing',
    'spawns.team_index',
    'spawns.gametypes',
    'items.*',
    'ticks',
]



def analyze_replay():

    last_tick = 0
    last_time = 0

    with open(r"V:\replays\2022-08-13_15-00-41.jsonl") as f:

        with open(r'V:\replays\output.json', 'w') as o:

            packets = []

            for line in f:
                data = json.loads(line)
                tick = data['game_time_info']['game_time']
                current_time = datetime.datetime.fromisoformat(data['current_time'])
                if last_tick and last_time:
                    if tick != last_tick + 1:
                        # print(f'{current_time}: unexpected tick {last_tick} -> {tick}')
                        pass
                    elif (duration := (current_time - last_time).microseconds) > 60000:
                        print(f'{tick}: tick took {duration/1000}ms')
                last_tick = tick
                last_time = current_time

                packet_data = {**data['game_update_data']['header'], **data['game_update_data']['data']}
                print(packet_data)
                packets.append(packet_data)

            print(json.dumps(packets))
            o.write(json.dumps(packets).replace(': nan', ': -1111111').replace(': NaN', ': -1111111'))
            print(json.dumps(packets)[14135:14150])
            # json.dump(packets, o)


def analyze_packets():
    """
    Read data dumped from an IDA breakpoint like this

        import datetime
        import base64
        ebx = GetRegValue("ebx")
        t = datetime.datetime.now()
        if(t.microsecond < 100000):
            print str(t) + ": " + hex(ebx)
            b = idc.GetManyBytes(ebx, 520, True)
            print(b)
            with open("V:/replays/out2.log", "ab") as f:
                f.write(base64.b64encode(b))
                f.write("\n")

    :return:
    """

    import base64

    with open('V:/replays/out2.log', 'rb') as f:
        with open('V:/replays/out3.bin', 'wb') as o:
            for line in f:
                print(base64.b64decode(line[:-1]).hex(' '))
                o.write(base64.b64decode(line[:-1]))


def test_compression():

    import zstandard as zstd

    filename = '2022-08-27_15-18-20.jsonl'

    with open(rf"V:\replays\{filename}", 'rb') as f:
        data_bytes = f.read()
        for level in range(22):
            with open(rf"V:\replays\{filename}.l{level}.zstd", 'wb') as o:
                compressor = zstd.ZstdCompressor(level=level)
                writer = compressor.stream_writer(o)
                writer.write(data_bytes)
                writer.flush(zstd.FLUSH_FRAME)


def extract_locations(replay_dict):

    db = DBConnector()

    records_to_insert = []

    # TODO: handle players joining and leaving
    for tick in replay_dict['ticks']:
        for player in tick['players']:
            if data := player['player_object_data']:
                # print(datetime.datetime.strptime(tick['current_time'], '%Y-%m-%d %H:%M:%S.%f'))
                record = dict(
                    time=datetime.datetime.strptime(tick['current_time'], '%Y-%m-%d %H:%M:%S.%f'),
                    player=player['player_index'],
                    tick=tick['game_time_info']['game_time'],
                    location=(data['x'], data['y'], data['z']),
                )
                records_to_insert.append((record['time'], record['player'], tick['game_time_info']['game_time'], *record['location']))

    db.insert_player_data_list(records_to_insert)


def should_keep_field(path, field_specs):
    # Check if the current path is part of the field specs or a wildcard (*)
    for spec in field_specs:
        if spec == path or spec.startswith(path + ".") or spec == path + ".*":
            return True
    return False


def strip_json_dotted(data, field_specs, path=""):
    if isinstance(data, dict):
        result = {}
        for key, value in data.items():
            new_path = f"{path}.{key}" if path else key
            if should_keep_field(new_path, field_specs):
                result[key] = strip_json_dotted(value, field_specs, new_path)
        return result

    elif isinstance(data, list):
        result = []
        for index, item in enumerate(data):
            new_path = f"{path}.*"  # Wildcard for list items
            if should_keep_field(new_path, field_specs):
                result.append(strip_json_dotted(item, field_specs, path))
        return result

    else:
        return data  # Base case: just return the value


def strip_json(data, fields):

    if isinstance(data, dict):
        # Check if we want to keep everything in this structure
        if fields == {}:
            return data  # Return the entire dictionary as-is

        # Recursively process the dictionary, but skip empty results
        result = {k: strip_json(v, fields.get(k, {})) for k, v in data.items() if k in fields}
        # Remove empty dictionaries
        # return {k: v for k, v in result.items() if v != {}}
        return {k: v for k, v in result.items()}

    elif isinstance(data, list):
        # Recursively process lists, and filter out empty or None results
        result = [strip_json(item, fields) for item in data]
        return [item for item in result if item != {}]

    else:
        # Base case: return the value for non-dict/list types
        return data


def process_compressed_replay(input_filename, output_folder):

    input_base_name = os.path.basename(input_filename).removesuffix('.json.zst')

    with open(input_filename, 'rb') as o:
        decompressor = zstd.ZstdDecompressor()
        data = o.read()
        json_bytes = decompressor.decompress(data, max_output_size=len(data))

        # only used for first/last tick, but could be used for everything else
        parser = simdjson.Parser()
        data = parser.parse(json_bytes)


    with open(os.path.join(output_folder, f'{input_base_name}_minimal.json'), 'w') as o:
        result = {
            "summary": data.get("summary").as_dict(),
            "game_meta": data.get("game_meta").as_dict(),
            "events": data.get("events").as_list(),
            "ticks": [data["ticks"][0].as_dict(), data["ticks"][-1].as_dict()] if data.get("ticks") else None,
        }
        json.dump(result, o)

    replay_dict = json.loads(json_bytes.decode('utf-8'))
    # extract_locations(replay_dict)

    stripped_replay = strip_json(replay_dict, necessary_fields)
    with open(os.path.join(output_folder, f'{input_base_name}_stripped.json'), 'w') as o:
        json.dump(stripped_replay, o)

    # stripped_replay = strip_json_dotted(replay_dict, necessary_fields_dotted)
    # with open(r'V:\replays\testtest_dotted.json', 'w') as o:
    #     json.dump(stripped_replay, o)

    with open(os.path.join(output_folder, f'{input_base_name}_stripped.json.zst'), 'wb') as o:
        compressor = zstd.ZstdCompressor()
        with o as compressed_file:
            compressed_file.write(compressor.compress(json.dumps(stripped_replay).encode('utf-8')))


def process_multiple(pattern, output_folder):

    file_paths = glob.glob(pattern)
    for file_path in file_paths:
        try:
            print(f'Processing {file_path}')
            process_compressed_replay(file_path, output_folder)
        except:
            print(f'Failed to process {file_path}')


if __name__ == '__main__':

    # test_compression()
    # process_compressed_replay(r"V:\replays\2024-11-19_20-26-03_final.json.zst")
    process_multiple(r'V:\replays\2024*_final.json.zst', r'V:\replays\processed')
