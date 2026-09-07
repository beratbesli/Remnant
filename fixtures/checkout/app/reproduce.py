import os
import sys

import psycopg
import redis


database_url = os.environ["FIXTURE_DATABASE_URL"]
redis_url = os.environ["FIXTURE_REDIS_URL"]

with psycopg.connect(database_url) as connection:
    with connection.cursor() as cursor:
        cursor.execute(
            """
            SELECT user_id, plan
            FROM subscriptions
            WHERE status = 'active' AND plan = 'pro'
            ORDER BY id
            LIMIT 1
            """
        )
        subscription = cursor.fetchone()

if subscription is None:
    print("checkout succeeds: no pro subscription")
    sys.exit(0)

user_id, plan = subscription
cache = redis.Redis.from_url(redis_url, decode_responses=True)
cached_plan = cache.get(f"user:{user_id}:plan")

if cached_plan is not None and cached_plan != plan:
    print(f"checkout bug reproduced: cache={cached_plan!r}, database={plan!r}")
    sys.exit(1)

print("checkout succeeds: cache agrees with database")
sys.exit(0)
