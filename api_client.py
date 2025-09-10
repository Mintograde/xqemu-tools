import os
from pprint import pprint

import httpx

base_url = os.getenv("API_BASE_URL")
api_key = os.getenv("API_KEY")

def send_game_status(status):

    if 'game_id' not in status:
        return

    headers = {
        "Content-Type": "application/json",
        'x-api-key': api_key,
    }

    try:
        response = httpx.post(f'{base_url}/game-status', json=status, headers=headers)
        response.raise_for_status()
    except Exception as e:
        pprint(status)
        raise


def watch_queue(game_status_queue):

    while True:
        status = game_status_queue.get()
        send_game_status(status)
        print(f"Sent status: {status}")


def start_client(game_status_queue):

    while True:
        try:
            watch_queue(game_status_queue)
        except Exception as e:
            print(f"API Client Error: {e}")
