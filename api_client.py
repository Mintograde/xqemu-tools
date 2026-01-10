import os
from pprint import pprint
import time
import httpx

base_url = os.getenv("API_BASE_URL")
api_key = os.getenv("API_KEY")


def send_game_status(client, status):

    if 'game_id' not in status:
        return

    try:
        response = client.post('/game-status', json=status)
        response.raise_for_status()
    except Exception as e:
        pprint(status)
        raise e


def watch_queue(game_status_queue):

    headers = {
        "Content-Type": "application/json",
        'x-api-key': api_key,
    }

    with httpx.Client(base_url=base_url, headers=headers, timeout=10.0) as client:
        while True:
            status = game_status_queue.get()
            send_game_status(client, status)
            print(f"Sent status: {status}")


def start_client(game_status_queue):

    while True:
        try:
            watch_queue(game_status_queue)
        except Exception as e:
            print(f"API Client Error: {e}")
            time.sleep(5)