@echo off
setlocal EnableExtensions
cd /d "%~dp0"

set "ACTION=%~1"
if "%ACTION%"=="" set "ACTION=start"

call :ensure_docker
if errorlevel 1 exit /b 1

if /I "%ACTION%"=="start" goto start
if /I "%ACTION%"=="restart" goto restart
if /I "%ACTION%"=="stop" goto stop
if /I "%ACTION%"=="logs" goto logs
if /I "%ACTION%"=="status" goto status
if /I "%ACTION%"=="spotify-login" goto spotify_login

echo CappyFM: unknown command "%ACTION%". Use: run.cmd [start^|restart^|stop^|logs^|status^|spotify-login] 1>&2
exit /b 1

:start
call :ensure_environment
if errorlevel 1 exit /b 1
echo CappyFM: building and starting the bot and Lavalink...
docker compose up -d --build
if errorlevel 1 exit /b 1
docker compose ps
echo.
echo CappyFM is running. Use "run.cmd logs" to watch it or "run.cmd stop" to shut it down.
exit /b 0

:restart
call :ensure_environment
if errorlevel 1 exit /b 1
echo CappyFM: rebuilding and restarting...
docker compose up -d --build --force-recreate
if errorlevel 1 exit /b 1
docker compose ps
exit /b 0

:stop
docker compose down
if errorlevel 1 exit /b 1
echo CappyFM is stopped. Persistent data remains in .\data.
exit /b 0

:logs
docker compose logs --tail=150 -f bot lavalink
exit /b %errorlevel%

:status
docker compose ps
exit /b %errorlevel%

:spotify_login
call :ensure_environment
if errorlevel 1 exit /b 1
echo CappyFM: building the one-time Spotify authorization helper...
echo CappyFM: make sure http://127.0.0.1:8888/callback is listed in your Spotify app's redirect URIs.
docker compose build bot
if errorlevel 1 exit /b 1
docker compose run --rm --no-deps -p 127.0.0.1:8888:8888 -e SPOTIFY_REDIRECT_URI=http://127.0.0.1:8888/callback -e SPOTIFY_REFRESH_TOKEN_FILE=/app/data/spotify-refresh-token bot cappy-bot --spotify-login
if errorlevel 1 exit /b 1
echo CappyFM: restarting the bot with Spotify playlist access...
docker compose up -d --build bot
exit /b %errorlevel%

:ensure_docker
where docker >nul 2>&1
if errorlevel 1 (
  echo CappyFM: Docker is not installed. Install Docker Desktop first. 1>&2
  exit /b 1
)

docker compose version >nul 2>&1
if errorlevel 1 (
  echo CappyFM: Docker Compose v2 is not available. 1>&2
  exit /b 1
)

docker info >nul 2>&1
if not errorlevel 1 exit /b 0

set "DOCKER_DESKTOP=%ProgramFiles%\Docker\Docker\Docker Desktop.exe"
if not exist "%DOCKER_DESKTOP%" (
  echo CappyFM: Docker Desktop is installed but is not running. Start it, then run this command again. 1>&2
  exit /b 1
)

echo CappyFM: starting Docker Desktop...
start "" "%DOCKER_DESKTOP%"
for /L %%I in (1,1,60) do (
  docker info >nul 2>&1
  if not errorlevel 1 goto docker_ready
  timeout /t 2 /nobreak >nul
)
echo CappyFM: Docker Desktop did not become ready within two minutes. 1>&2
exit /b 1

:docker_ready
echo CappyFM: Docker is ready.
exit /b 0

:ensure_environment
if not exist .env (
  copy /Y .env.example .env >nul
  echo CappyFM: created .env from .env.example. Fill in your secrets, then run run.cmd again. 1>&2
  exit /b 1
)

findstr /R /C:"^[ ]*DISCORD_TOKEN=[ ]*$" .env >nul
if not errorlevel 1 (
  echo CappyFM: DISCORD_TOKEN is empty in .env. 1>&2
  exit /b 1
)

if not exist data mkdir data
if not exist plugins mkdir plugins
exit /b 0
