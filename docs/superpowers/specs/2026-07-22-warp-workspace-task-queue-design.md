# Warp: cola local de tareas por workspace

## Objetivo

Incorporar en el fork de Masiriope una cola de ejecución ligera dentro de Warp. Sirve para preparar y lanzar trabajo de Codex o Claude Code sin duplicar Jira ni sustituir las sesiones normales de terminal.

El MVP es local y manual. Jira no forma parte de la primera entrega.

## Límites del MVP

Incluido:

- Cola de tareas Markdown por workspace.
- Título, prioridad, contexto y capturas pegadas o adjuntadas.
- Lanzamiento explícito desde el detalle de cada tarea con `codexauto` o `claudeauto`.
- Seguimiento local de estados y vínculo con la sesión de Warp creada.
- Descubrimiento de workspaces desde dos carpetas raíz locales.

Excluido:

- Lectura, creación, transición o actualización de tickets Jira.
- Sincronización de datos entre máquinas.
- Clasificación de sesiones iniciadas manualmente con `codexauto` o `claudeauto`.
- Reemplazar la UI de chat o la experiencia actual de terminal.

## Experiencia de usuario

### Sesiones y tareas

El panel vertical existente alterna entre dos modos:

- **Sesiones:** conserva la lista y el comportamiento actuales de Warp.
- **Tareas:** muestra la cola del workspace seleccionado y el detalle de la tarea a la derecha.

No se modifica el área de chat ni la terminal al entrar en Tareas. Una fila de tarea sólo selecciona la tarea: no inicia agentes desde la barra lateral.

### Workspaces

Las tareas siempre pertenecen a un workspace. El selector de workspace vive en el diálogo **Nueva tarea** y muestra la ruta que se utilizará para abrir el agente.

Los workspaces se descubren como directorios hijos directos de estas fuentes:

| Grupo | Carpeta raíz |
| --- | --- |
| GitHub | `~/Documents/GitHub` |
| Proyectos activos · Masiriope | `~/Desktop/01_Proyectos_Activos` |

El selector agrupa visualmente ambas fuentes y se puede refrescar. Dos carpetas con el mismo nombre siguen siendo workspaces distintos porque su identidad se basa en la ruta canónica, no en el nombre visible.

**Sin workspace** es una opción virtual exclusiva para conversaciones generales. Su directorio de trabajo es `~` (`/Users/sebas`) y no posee ni muestra una cola de tareas.

El MVP no obliga a dar de alta cada workspace uno a uno ni escribe archivos dentro de sus proyectos. La configuración de fuentes adicionales queda fuera de alcance por ahora.

### Crear una tarea

Desde Tareas, **Nueva tarea** abre un diálogo interno con la apariencia y componentes nativos de Warp, no una ventana independiente de macOS. Incluye:

1. Workspace obligatorio, con nombre y ruta visibles.
2. Título obligatorio.
3. Prioridad: Alta, Normal o Baja.
4. Contexto opcional en formato texto/Markdown.
5. Adjuntos opcionales. `⌘V` puede añadir texto y varias capturas; cada captura se previsualiza y se puede retirar antes de crear.

La tarea se crea en estado **Pendiente** dentro de la cola del workspace seleccionado.

### Detalle y lanzamiento

Al seleccionar una tarea se presenta, a la derecha, título, prioridad, contexto, previsualizaciones de capturas, estado y acciones. Sólo ese detalle ofrece:

- **Abrir con Codex**
- **Abrir con Claude Code**

Cada acción abre una nueva sesión normal de Warp en el directorio guardado para el workspace. No reutiliza, cierra ni modifica ninguna sesión existente.

Warp ejecutará los wrappers que ya usa Sebastián con un primer prompt interactivo, equivalente a su flujo manual actual:

```sh
codexauto "Lee y ejecuta la tarea en /ruta/absoluta/LOCAL-004/task.md, incluidos sus adjuntos."
claudeauto "Lee y ejecuta la tarea en /ruta/absoluta/LOCAL-004/task.md, incluidos sus adjuntos."
```

Los comandos se deben construir con escapes de shell correctos. No se presupone un argumento inexistente como `--task`. Los wrappers actuales aceptan un prompt posicional:

- `codexauto` conserva el directorio inicial y delega en `codex -C <directorio> <prompt>`.
- `claudeauto` delega en Claude Code en el directorio de trabajo actual.

El Markdown contiene enlaces relativos a sus adjuntos. El prompt señala la ruta absoluta del `task.md`, de modo que el agente puede leer el contexto y localizar las capturas sin depender del portapapeles.

### Sesión vinculada

Una sesión creada desde una tarea se registra internamente como vinculada a esa tarea. Mientras siga activa:

- aparece encima de las sesiones ordinarias, bajo el separador **Tareas en curso**;
- mantiene el mismo aspecto de fila que el resto de Sesiones;
- no necesita iconos de Codex ni Anthropic: muestra título de tarea, estado y referencia local;
- permite volver al detalle de la tarea.

El detalle de la tarea, a su vez, permite **Abrir sesión** cuando ya existe una sesión vinculada activa. No lanzará una segunda sesión accidentalmente.

La relación se crea exclusivamente al usar las acciones de una tarea. Una sesión donde el usuario escriba manualmente `codexauto` o `claudeauto` se mantiene completamente ajena a la cola.

## Datos locales

La build del fork utiliza un directorio de datos propio, separado de Warp estable y de los repositorios de trabajo. La ubicación concreta seguirá la identidad de aplicación separada de la build de Masiriope; bajo ella, el módulo usa esta estructura lógica:

```text
TaskQueue/v1/
  sources.json
  workspaces.json
  tasks/
    <workspace-id>/
      <task-id>/
        task.md
        attachments/
          captura-001.png
          captura-002.png
```

`sources.json` registra las dos rutas raíz configuradas. `workspaces.json` es un índice regenerable de las carpetas descubiertas y de sus identificadores estables. La fuente de verdad de la tarea es el Markdown.

Ejemplo de `task.md`:

```markdown
---
id: LOCAL-004
workspace_id: path-sha256
workspace_path: /Users/sebas/Documents/GitHub/MGA-Portal
title: Revisar discrepancias de firmas
priority: high
status: pending
created_at: 2026-07-22T10:00:00+02:00
linked_session_id: null
---

# Revisar discrepancias de firmas

Comprobar las incidencias reportadas entre las máquinas de intranet.

## Adjuntos

- [captura-001.png](attachments/captura-001.png)
- [captura-002.png](attachments/captura-002.png)
```

Los adjuntos se copian de forma atómica antes de publicar la tarea en el índice. Si falla una copia, la creación se revierte y no queda una tarea incompleta.

## Estados y recuperación

```text
Pendiente --lanzar--> En curso --salida correcta--> Para revisar --confirmación manual--> Hecha
                                  \--error o ruta inválida--> Atención requerida
```

- **Pendiente:** tarea preparada, aún sin agente asociado.
- **En curso:** hay una sesión vinculada activa.
- **Para revisar:** el agente terminó correctamente; no significa que el trabajo esté validado.
- **Hecha:** sólo la marca el usuario.
- **Atención requerida:** no se pudo crear o ejecutar la sesión, falta la carpeta del workspace, o el proceso termina con error. El detalle conserva el motivo y permite reintentar.

No se hace ningún cambio automático en Jira, aunque una futura integración podrá añadir importación de tickets y adjuntos.

## Integración con el código de Warp

El trabajo se integrará en los patrones ya existentes del repositorio:

- El cambio Sesiones/Tareas se implementará dentro del panel de pestañas verticales existente (`app/src/workspace/view/vertical_tabs.rs`), sin introducir un panel paralelo.
- La apertura del diálogo de tarea reutilizará los componentes y estilos de diálogo de Warp (`app/src/ui_components/dialog.rs`), con un formulario de tarea específico en lugar de `NativeModal`.
- La acción de lanzamiento reutilizará la creación normal de una terminal/pestaña de Warp y registrará el identificador de esa sesión antes de inyectar el comando.
- El almacén local, el descubrimiento de workspaces y el modelo Markdown se aislarán de las vistas para que puedan probarse sin UI.

## Manejo de errores

- Si un workspace desaparece o se mueve, se muestra **Ruta no disponible** y las acciones de agente permanecen desactivadas hasta refrescar o corregir la fuente.
- Si el comando de agente no puede iniciarse, la tarea pasa a Atención requerida sin borrar contexto ni adjuntos.
- Si una tarea tiene una sesión activa, se ofrece abrirla; relanzar exige una decisión explícita del usuario.
- Los errores de parseo de Markdown o del índice se aíslan por tarea y no impiden ver las demás colas.

## Pruebas y seguridad de desarrollo

Se creará y probará una build independiente de la aplicación actual, con identidad y datos separados. Está prohibido cerrar, reiniciar, sustituir o reutilizar el Warp que Sebastián tiene abierto, sus conversaciones, sus pestañas o sus procesos.

Cobertura prevista:

- Descubrimiento y refresco de los directorios hijos de las dos fuentes raíz.
- Identidad de workspaces con nombres repetidos y rutas inválidas.
- Serialización, lectura, migración y recuperación de `task.md` y adjuntos.
- Transiciones de estado, incluido error de proceso y sesión ya activa.
- Composición y escape seguro de los comandos `codexauto` y `claudeauto`.
- Flujo manual de UI con un perfil de desarrollo vacío: crear tarea, pegar varias capturas, lanzar ambos agentes, volver a la sesión, finalizar y marcar hecha.

## Entrega

El diseño y la implementación se trabajan en el fork `Masiriope/warp`, en ramas dedicadas y con commits enviados a `origin`. Antes de cualquier instalación que pudiera sustituir a Warp estable se solicitará autorización explícita.
