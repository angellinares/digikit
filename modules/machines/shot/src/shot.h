/* shot.h: the internal header of the SHOT module. */
#ifndef SHOT_H
#define SHOT_H
#include <dk/api.h>
#include <dk/machine.h>
#include <dk/params.h>
#include <dk/sample_track.h>
#include <dk/sample_region.h>
#include <dk/widgets.h>
#include <dk/ui.h>
#include "shot_params.h"
#include "sample_view.h"

extern sample_view shot_samples; /* the pool view of both pages (module.c) */

void shot_controls_draw(const dk_page_frame_v1 *f);
void shot_range_draw(const dk_page_frame_v1 *f);
void shot_range_event(const dk_page_event_v1 *event);

#endif
