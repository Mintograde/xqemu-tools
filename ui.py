import queue
from pprint import pprint

import orjson
import dearpygui.dearpygui as dpg


class Diff:
    """
    On the other end of the queue, these strings will be converted to ints.
    address, value, and length can be given either in hex (with prefix 0x) or in decimal
    address values are in guest
    """
    address: str
    value: str
    length: str

    def __init__(self, address, value, length):
        self.address = address
        self.value = value
        self.length = length

    @classmethod
    def from_diff_string(cls, s):
        """
        Constructs a Diff object from a single line of an IDA diff string
        Addresses are converted from file offsets to memory offsets
        TODO: return a list of Diff objects if given an entire multiline diff string?
        TODO: if handling multiline strings, also detect sequential runs of addresses
        """

        address, _, value = s.strip().split()
        address = hex(int(address.removesuffix(':'), 16) + 0x10000)
        value = f'0x{value}'
        length = '1'
        return cls(address, value, length)

    def as_dict(self):

        return dict(
            address=self.address,
            value=self.value,
            length=self.length,
        )

    def __repr__(self):

        return f'<Diff: address:{self.address} value:{self.value} length:{self.length}>'


def handle_write_clicked(sender, app_data, user_data):

    write_queue_from_ui = user_data

    # 0x2F90C4
    # 1
    # 1

    write_queue_from_ui.put(dict(
        address=dpg.get_value('write_address'),
        value=dpg.get_value('write_value'),
        length=dpg.get_value('write_length'),
    ))

    dpg.set_value('write_address', '')
    dpg.set_value('write_value', '')
    dpg.set_value('write_length', '')


def send_preset(diffs, write_queue):

    print('Sending diffs through queue')
    for diff in diffs:
        write_queue.put(diff.as_dict())


def handle_solobox_clicked(sender, app_data, user_data):
    """
    Changes the third of three instances of this in xemu memory
    See network_game_server_update_countdown()
        network_game_server_allow_game_start(TRUE) needs to be called first,
            which sets server->countdown struct to 0 and server->countdown.paused to 1
            (struct network_game_server)
    """

    diff_string = '''
        # always_allow_start_game.dif
        0008C514: 32 B0
        0008C515: C0 01
        
        # startgame_ignore-teamcheck_ignore-endgameteams.dif
        0008C0D2: 01 00
        000F7DEA: 0F 90
        000F7DEB: 84 90
        000F7DEC: 92 90
        000F7DED: 01 90
        000F7DEE: 00 90
        000F7DEF: 00 90
    '''

    diffs = [Diff.from_diff_string(s) for s in diff_string.splitlines() if s and ':' in s and not s.strip().startswith('#')]
    send_preset(diffs, user_data)


def start_ui(game_info_queue_for_ui, write_queue_from_ui):

    # TODO: try out 3d stuff https://github.com/hoffstadt/DearPyGui/discussions/1416
    # TODO: look at some examples:
    #       https://github.com/Magic-wei/DearBagPlayer
    #       https://github.com/hoffstadt/DearPyGui/wiki/Dear-PyGui-Showcase#hyperspectral-imaging-acquisition

    # TODO: compare with https://github.com/pyimgui/pyimgui

    dpg.create_context()

    # hidpi workarounds: https://github.com/hoffstadt/DearPyGui/issues/1380
    # Include the following code before showing the viewport/calling `dearpygui.dearpygui.show_viewport`.
    # import ctypes
    # ctypes.windll.shcore.SetProcessDpiAwareness(2)
    # with dpg.font_registry():
    #     dpg.add_font("JetBrainsMono-Regular.ttf", 15 * 2, tag="ttf-font")

    dpg.create_viewport(title='Xemu Memory Watcher', width=1680, height=1050)

    with dpg.window(label="info"):
        dpg.add_input_text(tag="filter", label="filter")
        dpg.add_input_text(tag='player_info', width=800, height=900, multiline=True, readonly=True)
        # dpg.bind_item_font(dpg.last_item(), "ttf-font")

    with dpg.window(label="positions", pos=(900, 0)):
        # dpg.set_global_font_scale(0.5)
        with dpg.plot(label='positions', width=600, height=600):
            dpg.add_plot_axis(dpg.mvXAxis, label="x", tag="x_axis")
            dpg.set_axis_limits(dpg.last_item(), -20, 20)
            dpg.add_plot_axis(dpg.mvYAxis, label="y", tag="y_axis")
            dpg.set_axis_limits(dpg.last_item(), -20, 20)
            dpg.add_scatter_series([1], [1], parent="y_axis", tag="series")
            dpg.add_scatter_series([1], [1], parent="y_axis", tag="item_series")
            dpg.add_scatter_series([1], [1], parent="y_axis", tag="object_series")

    perf_x = []
    perf_y = []
    perf_y_2 = []
    perf_y_3 = []
    memory_mbytes_count_y = []

    with dpg.window(label='performance', pos=(900, 550)):
        with dpg.plot(label="performance", height=400, width=600):
            # optionally create legend
            dpg.add_plot_legend()

            # REQUIRED: create x and y axes
            dpg.add_plot_axis(dpg.mvXAxis, label="x", tag="perf_x_axis")
            dpg.add_plot_axis(dpg.mvYAxis, label="y", tag="perf_y_axis")
            dpg.add_plot_axis(dpg.mvYAxis, label="y", tag="counts_y_axis")

            # series belong to a y axis
            dpg.add_line_series(perf_x, perf_y, label="game_info_ms", parent="perf_y_axis", tag="series_tag")
            dpg.add_line_series(perf_x, perf_y_2, label="loop_ms", parent="perf_y_axis", tag="series_tag_2")
            dpg.add_line_series(perf_x, perf_y_3, label="post_steps_ms", parent="perf_y_axis", tag="series_tag_3")
            dpg.add_line_series(perf_x, memory_mbytes_count_y, label="memory_mbytes", parent="counts_y_axis", tag="series_tag_4")

    # dpg.show_metrics()

    with dpg.window(label='editor', pos=(900, 550)):

        dpg.add_button(tag='send_solobox_startgame', label='Allow solo box start', callback=handle_solobox_clicked, user_data=write_queue_from_ui)
        dpg.add_input_text(tag='write_address', label='address')
        dpg.add_input_text(tag='write_value', label='value')
        dpg.add_input_text(tag='write_length', label='length')
        dpg.add_button(tag='write_button', label='write', callback=handle_write_clicked, user_data=write_queue_from_ui)

    dpg.setup_dearpygui()
    dpg.show_viewport()

    while dpg.is_dearpygui_running():

        try:

            game_info = game_info_queue_for_ui.get(block=False)

            # dpg.set_value('player_info', value=pformat(game_info, sort_dicts=False))

            # orjson gives a roughly 3x speedup here (from 500 loops/tick to 1600 loops/tick)
            # NOTE: OPT_NON_STR_KEYS is needed when damage_counts dict is active (but is slower)
            #       https://github.com/ijl/orjson#opt_non_str_keys
            player_info_string = orjson.dumps(game_info, option=orjson.OPT_INDENT_2 | orjson.OPT_NON_STR_KEYS).decode()
            # TODO: optionally if matching line starts a dict or list, include contents
            # TODO: optionally include containing object for matched lines
            if filter_string := dpg.get_value('filter'):
                player_info_string = '\n'.join([line for line in player_info_string.splitlines() if filter_string in line])
            dpg.set_value('player_info', value=player_info_string)

            x_positions = []
            y_positions = []

            x_item_positions = []
            y_item_positions = []

            x_object_positions = []
            y_object_positions = []

            if 'items' in game_info and game_info['items']:
                for item in game_info['items']:
                    x_item_positions.append(item['item_x'])
                    y_item_positions.append(item['item_y'])
                dpg.set_value('item_series', [x_item_positions, y_item_positions])
            if 'objects' in game_info and game_info['objects']:
                for obj in game_info['objects']:
                    x_object_positions.append(obj['x'])
                    y_object_positions.append(obj['y'])
                dpg.set_value('object_series', [x_object_positions, y_object_positions])
            if 'players' in game_info and game_info['players']:
                for player in game_info['players']:
                    if player['player_object_data']:
                        x_positions.append(player['player_object_data']['x'])
                        y_positions.append(player['player_object_data']['y'])
                dpg.set_value('series', [x_positions, y_positions])
            if 'performance' in game_info:

                # reset counters for new game
                if len(perf_x) > 0 and game_info['game_time_info']['game_time'] < perf_x[-1]:
                    perf_x = []
                    perf_y = []
                    perf_y_2 = []
                    perf_y_3 = []
                    memory_mbytes_count_y = []

                perf_x.append(game_info['game_time_info']['game_time'])
                perf_y.append(game_info['performance']['game_info_time'])
                perf_y_2.append(game_info['performance']['loop_time'])
                perf_y_3.append(game_info['performance']['post_steps_ms'])
                memory_mbytes_count_y.append(game_info['performance']['memory_mbytes'])
                graph_width = 1800
                if len(perf_x) > graph_width:
                    perf_x = perf_x[len(perf_x)-graph_width:]
                    perf_y = perf_y[len(perf_y)-graph_width:]
                    perf_y_2 = perf_y_2[len(perf_y_2)-graph_width:]
                    perf_y_3 = perf_y_3[len(perf_y_3)-graph_width:]
                    memory_mbytes_count_y = memory_mbytes_count_y[len(memory_mbytes_count_y)-graph_width:]
                dpg.set_value('series_tag', [perf_x, perf_y])
                dpg.set_value('series_tag_2', [perf_x, perf_y_2])
                dpg.set_value('series_tag_3', [perf_x, perf_y_3])
                dpg.set_value('series_tag_4', [perf_x, memory_mbytes_count_y])
                dpg.fit_axis_data("perf_x_axis")
                dpg.fit_axis_data("counts_y_axis")
                dpg.set_axis_limits("perf_y_axis", 0, 100)
                # dpg.fit_axis_data("perf_y_axis")

        except queue.Empty:

            pass

        dpg.render_dearpygui_frame()

    dpg.destroy_context()


if __name__ == '__main__':

    # handle_solobox_clicked(None, None, None)
    start_ui(queue.Queue(), queue.Queue())
