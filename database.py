import datetime

import psycopg


class DBConnector:

    # TODO: look into questdb https://questdb.io/get-questdb/
    SRID = 3857  # EPSG:3857 = Google Mercator, EPSG:3395 = World Mercator
    CONNECTION_STRING = "postgres://postgres:postgres@localhost:5432/halo"

    def __init__(self):

        self.conn = None
        self.connect()

    def connect(self):

        self.conn = psycopg.connect(self.CONNECTION_STRING)

    def create_initial_tables(self):
        """
        Some info on srid:
            https://gis.stackexchange.com/questions/265797/srid-for-basic-x-y-coordinate-system
            https://gis.stackexchange.com/questions/64252/postgis-spatial-reference-id-srid-for-regular-cartesian-coordinate-system?rq=1
        """

        table_query = """CREATE TABLE player_data (
                           time TIMESTAMPTZ NOT NULL,
                           player INTEGER,
                           tick INTEGER,
                           location geometry(POINTZ, 3857),
                           x REAL,
                           y REAL,
                           z REAL
                           );"""

        hypertable_query = "SELECT create_hypertable('player_data', 'time');"

        cur = self.conn.cursor()
        cur.execute(table_query)
        cur.execute(hypertable_query)
        self.conn.commit()
        cur.close()

    def insert_player_data(self, data):

        with self.conn.cursor() as cur:
            query = "INSERT INTO player_data (time, player, tick, location) VALUES (%s, %s, %s, ST_SetSRID(ST_MakePoint(%s, %s, %s), 3857))"
            query_vars = [data['time'], data['player'], data['tick'], *data['location']]
            cur.execute(query, query_vars)
            self.conn.commit()

    def insert_player_data_list(self, data_list):

        with self.conn.cursor() as cur:
            query = "INSERT INTO player_data (time, player, tick, location) VALUES (%s, %s, %s, ST_SetSRID(ST_MakePoint(%s, %s, %s), 3857))"
            cur.executemany(query, data_list)
            self.conn.commit()


    def insert_test_data(self):
        """
        https://gis.stackexchange.com/questions/108533/insert-a-point-into-postgis-using-python
        https://gis.stackexchange.com/questions/193446/python-to-postgres-insert-geometry-syntax
        """

        data = dict(
            time=datetime.datetime.now(),
            player=0,
            tick=15039,
            location=(22, 11, 5),
        )
        self.insert_player_data(data)


    # def find_points_within_box(self, x, y, z):
    #     """
    #     From comment here, min/max should be faster than spatial contains queries
    #         https://gis.stackexchange.com/a/126636
    #     :param y:
    #     :param z:
    #     :return:
    #     """
    #
    #     with self.conn.cursor() as cur:
    #         query =


if __name__ == '__main__':

    db = DBConnector()
    # db.create_initial_tables()
    db.insert_test_data()
