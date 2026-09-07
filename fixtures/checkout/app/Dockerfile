FROM python:3.12-slim

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY seed.py reproduce.py ./

CMD ["python", "-c", "import time; time.sleep(3600)"]
