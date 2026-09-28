#include "rowdelegate.h"

#include <QFontMetrics>
#include <QIcon>
#include <QPainter>
#include <QPainterPath>
#include <QPen>
#include <QStringList>
#include <QVector>


namespace {

/* The icon slot is 16px and always reserved, whether an icon arrived for this row or not. The product
 * resolves shell icons on a worker thread and answers later, so a row that reserved nothing would
 * slide its name left the moment the answer landed - and text that moves under the cursor cannot be
 * clicked. */
constexpr int kIconSide = 16;
constexpr int kIconGap = 10;
/* Ant Design's Tag: 12px text, 7px of horizontal padding, a 4px radius, a hairline of its own. */
constexpr int kTagPadding = 7;
constexpr int kTagHeight = 20;
constexpr int kRightGap = 8;
/* Between two tag chips in the band, and between the band and the name. The gap inside a band is the
 * 4px `paddingXS` Ant Design puts between two tags; the wider one is what keeps the name from reading
 * as a fourth chip. */
constexpr int kTagGap = 4;
constexpr int kNameTagGap = 8;
/* The narrowest a name may be squeezed by a tag band. Sixty pixels is four characters plus an
 * ellipsis: below that the row no longer says which file it is, which is the one thing a row exists
 * to say. A tag is a fact *about* the file, so when the two cannot both have room it is the tags that
 * get dropped - and the band's other bound, two thirds of what is left, is what makes that true even
 * when the floor would have allowed more. */
constexpr int kNameFloor = 60;

/* Two line glyphs, for the row that has no icon yet. In the product shell that is a real state rather
 * than a missing library: shell glyphs are resolved on a worker thread and answered later, so every
 * row starts out here, and a 16px slot that paints nothing on those rows reads as a bug rather than
 * as a wait. It is also the whole answer when no icon cache is wired at all - the model then hands
 * back an empty DecorationRole, which is what the design sample does on purpose. */
void drawGlyph(QPainter *painter, const QRectF &slot, bool directory, const QColor &ink) {
    QPen pen(ink);
    pen.setWidthF(1.3);
    pen.setJoinStyle(Qt::RoundJoin);
    pen.setCapStyle(Qt::RoundCap);
    painter->setPen(pen);
    painter->setBrush(Qt::NoBrush);
    const QRectF box = slot.adjusted(1.5, 2.5, -1.5, -2.5);
    if (directory) {
        QPainterPath folder;
        folder.moveTo(box.left(), box.top() + 2.5);
        folder.lineTo(box.left(), box.bottom() - 1.0);
        folder.lineTo(box.right(), box.bottom() - 1.0);
        folder.lineTo(box.right(), box.top() + 1.0);
        folder.lineTo(box.left() + 4.5, box.top() + 1.0);
        folder.lineTo(box.left() + 3.0, box.top() - 1.0 + 1.5);
        folder.closeSubpath();
        painter->drawPath(folder);
        return;
    }
    /* A page with a folded corner - the file shape the icon set uses everywhere else. */
    const qreal fold = 3.4;
    QPainterPath page;
    page.moveTo(box.left(), box.top());
    page.lineTo(box.right() - fold, box.top());
    page.lineTo(box.right(), box.top() + fold);
    page.lineTo(box.right(), box.bottom());
    page.lineTo(box.left(), box.bottom());
    page.closeSubpath();
    painter->drawPath(page);
    painter->drawLine(QPointF(box.right() - fold, box.top()),
                      QPointF(box.right() - fold, box.top() + fold));
    painter->drawLine(QPointF(box.right() - fold, box.top() + fold),
                      QPointF(box.right(), box.top() + fold));
}

} /* namespace */

RowDelegate::RowDelegate(const Theme *theme, const RowRoles &roles, QObject *parent)
    : QStyledItemDelegate(parent), m_theme(theme), m_roles(roles) {}

QSize RowDelegate::sizeHint(const QStyleOptionViewItem &, const QModelIndex &) const {
    return QSize(0, m_theme->tokens().rowHeight);
}

int RowDelegate::chipWidth(const QString &text) const {
    const Tokens &t = m_theme->tokens();
    if (text.isEmpty()) {
        return 0;
    }
    QFontMetrics metrics(m_theme->fontAt(t.fontSM));
    return metrics.horizontalAdvance(text) + 2 * kTagPadding;
}

int RowDelegate::paintTag(QPainter *painter, const QRect &row, const QString &text, int right) const {
    const Tokens &t = m_theme->tokens();
    if (text.isEmpty()) {
        return right;
    }
    painter->setFont(m_theme->fontAt(t.fontSM));
    const int width = chipWidth(text);
    const QRect chip(right - width, row.center().y() - kTagHeight / 2, width, kTagHeight);
    const QColor ground = flattenOnto(t.fillSecondary, t.bgContainer);
    painter->setPen(ground);
    painter->setBrush(ground);
    painter->drawRoundedRect(chip, t.radiusSM, t.radiusSM);
    /* The chip carries its own hairline rather than inheriting the row's: it sits on the container,
     * on the hover fill and on the selected fill, and one border colour cannot agree with all three. */
    painter->setPen(flattenOnto(t.borderSecondary, t.bgContainer));
    painter->drawRoundedRect(chip, t.radiusSM, t.radiusSM);
    painter->setPen(t.textSecondary);
    painter->drawText(chip, Qt::AlignCenter, text);
    return chip.left() - kRightGap;
}

int RowDelegate::tagBandWidth(const QStringList &tags) const {
    int total = 0;
    for (const QString &tag : tags) {
        total += chipWidth(tag) + kTagGap;
    }
    return tags.isEmpty() ? 0 : total - kTagGap;
}

int RowDelegate::tagsFitting(const QStringList &tags, int left, int right, int *groupWidth) const {
    *groupWidth = 0;
    if (tags.isEmpty()) {
        return 0;
    }
    /* How much room the band may take. Two bounds, and the tighter one wins: the name keeps its floor,
     * and the name keeps at least a third of what is left whatever the floor allows - a file with eight
     * tags should not turn a listing into a row of chips with a stub of a name in front. */
    const int available = right - left;
    const int room = qMax(0, qMin(available - kNameFloor, available - available / 3));
    if (room <= 0) {
        return 0;
    }

    /* Keep as many tags as fit, counting the ones that do not into a single `+N` chip. The counter is
     * measured on every pass, because the answer is not monotone in the naive way: dropping a tag makes
     * room for the counter, and the counter is narrower than most tags.
     *
     * It stops at one tag rather than at zero, because a band of nothing but `+6` says "there are tags
     * here" while showing none of them - which is a chip that costs the name its room to describe a
     * field the row has then removed. Either the row can show a tag, or it shows the name. */
    int kept = tags.size();
    int taken = tagBandWidth(tags);
    while (kept > 1 && taken > room) {
        --kept;
        /* `kept` gaps, not `kept - 1`: the last one is between the final chip and the counter. */
        taken = kTagGap * kept + chipWidth(QStringLiteral("+%1").arg(tags.size() - kept));
        for (int chip = 0; chip < kept; ++chip) {
            taken += chipWidth(tags.at(chip));
        }
    }
    if (taken > room) {
        /* Not even one tag plus the counter fits. */
        return 0;
    }
    *groupWidth = taken;
    return kept;
}

int RowDelegate::paintTags(QPainter *painter, const QRect &row, const QStringList &tags, int left,
                           int right) const {
    const Tokens &t = m_theme->tokens();
    int groupWidth = 0;
    const int kept = tagsFitting(tags, left, right, &groupWidth);
    if (kept == 0) {
        return right;
    }
    painter->setFont(m_theme->fontAt(t.fontSM));
    int cursor = right - groupWidth;
    const QColor ground = flattenOnto(t.primaryBg, t.bgContainer);
    const QColor edge = flattenOnto(t.tagBorder, t.bgContainer);
    for (int chip = 0; chip < kept; ++chip) {
        const int width = chipWidth(tags.at(chip));
        const QRect box(cursor, row.center().y() - kTagHeight / 2, width, kTagHeight);
        painter->setPen(ground);
        painter->setBrush(ground);
        painter->drawRoundedRect(box, t.radiusSM, t.radiusSM);
        painter->setPen(edge);
        painter->drawRoundedRect(box, t.radiusSM, t.radiusSM);
        painter->setPen(t.accentText);
        painter->drawText(box, Qt::AlignCenter, tags.at(chip));
        cursor += width + kTagGap;
    }
    if (kept < tags.size()) {
        const QString more = QStringLiteral("+%1").arg(tags.size() - kept);
        const QRect box(cursor, row.center().y() - kTagHeight / 2, chipWidth(more), kTagHeight);
        /* The counter is not a tag, so it is not drawn like one: it takes the metadata colours - the
         * same fill and hairline the kind chip uses - and the tertiary text. A row whose third chip
         * is grey says "these two are the tags, and there are more"; one where all three are blue
         * says the file has a tag literally named `+2`. */
        const QColor quiet = flattenOnto(t.fillSecondary, t.bgContainer);
        painter->setPen(quiet);
        painter->setBrush(quiet);
        painter->drawRoundedRect(box, t.radiusSM, t.radiusSM);
        painter->setPen(flattenOnto(t.borderSecondary, t.bgContainer));
        painter->drawRoundedRect(box, t.radiusSM, t.radiusSM);
        painter->setPen(t.textTertiary);
        painter->drawText(box, Qt::AlignCenter, more);
    }
    return right - groupWidth - kNameTagGap;
}

void RowDelegate::paint(QPainter *painter, const QStyleOptionViewItem &option,
                        const QModelIndex &index) const {
    const Tokens &t = m_theme->tokens();
    const bool selected = option.state & QStyle::State_Selected;
    const bool hovered = option.state & QStyle::State_MouseOver;
    const bool directory = index.data(m_roles.directory).toBool();

    painter->save();
    painter->setRenderHint(QPainter::Antialiasing, true);
    painter->setRenderHint(QPainter::TextAntialiasing, true);
    painter->fillRect(option.rect, t.bgContainer);

    /* A delegate that paints its own text also paints its own ground: once the view hands the row
     * over, no `::item` rule is applied to it any more. */
    const QRect ground = option.rect.adjusted(t.xxs, 0, -t.xxs, 0);
    if (selected || hovered) {
        painter->setPen(Qt::NoPen);
        painter->setBrush(flattenOnto(selected ? t.bgSelected : t.bgHover, t.bgContainer));
        painter->drawRoundedRect(ground, t.radius, t.radius);
    }

    const QString name = index.data(m_roles.name).toString();
    const QString kind = directory ? QString() : index.data(m_roles.kind).toString();
    const QStringList tags = index.data(m_roles.tags).toStringList();
    const qint64 bytes = index.data(m_roles.size).toLongLong();

    const QIcon icon = qvariant_cast<QIcon>(index.data(Qt::DecorationRole));
    const QRect slot(cursorRect(ground));
    if (!icon.isNull()) {
        icon.paint(painter, slot, Qt::AlignCenter);
    } else {
        drawGlyph(painter, QRectF(slot), directory, t.textTertiary);
    }
    int cursor = slot.right() + 1 + kIconGap;

    /* Right to left, because the name is the thing that gives: the fields to its right keep their
     * width, and the text is elided into what is left of it. */
    int boundary = ground.right() - t.sm;

    QFontMetrics baseMetrics(m_theme->fontAt(t.fontBase));
    /* A directory has no size to print. Its `size` field is zero because nothing wrote a byte count
     * into it, and "0 B" on every folder is a row of the list saying something untrue. */
    const QString size = (!directory && bytes >= 0)
                             ? m_roles.formatSize(static_cast<quint64>(bytes))
                             : QString();
    const int sizeCost = size.isEmpty() ? 0 : baseMetrics.horizontalAdvance(size) + t.md;
    const int kindCost = kind.isEmpty() ? 0 : chipWidth(kind) + kRightGap;

    /* What the band would occupy and how many tags it would show, for a given set of fields drawn to
     * its right. */
    auto laidOut = [&](bool showKind, bool showSize, int *width) {
        const int right = boundary - (showKind ? kindCost : 0) - (showSize ? sizeCost : 0);
        int group = 0;
        const int kept = tagsFitting(tags, cursor, right, &group);
        *width = kept > 0 ? group + kNameTagGap : 0;
        return kept;
    };

    bool showKind = !kind.isEmpty();
    bool showSize = !size.isEmpty();
    int band = 0;
    int kept = laidOut(showKind, showSize, &band);
    /* The ladder the fields right of the name climb down when the row runs out of room: the kind chip
     * first, because the glyph and the extension in the name already say what it says, then the size,
     * because a number is read slower than a name. The name's floor is the promise the row keeps; a
     * column narrower than that floor loses everything but the name rather than printing a row with no
     * name in it - which is what a 150px column used to do to a file with a type chip. */
    while (boundary - (showKind ? kindCost : 0) - (showSize ? sizeCost : 0) - band - cursor
           < kNameFloor) {
        if (showKind) {
            showKind = false;
        } else if (showSize) {
            showSize = false;
        } else {
            break;
        }
        kept = laidOut(showKind, showSize, &band);
    }
    /* The chip also yields when giving it up is all the tags were missing. A row with room for its name
     * either way should still show every tag it has. */
    if (showKind && kept < tags.size()) {
        int wholeBand = 0;
        if (laidOut(false, showSize, &wholeBand) == tags.size()) {
            showKind = false;
            band = wholeBand;
            kept = tags.size();
        }
    }

    if (showKind) {
        boundary = paintTag(painter, ground, kind, boundary);
    }
    if (showSize) {
        painter->setFont(m_theme->fontAt(t.fontBase));
        const QRect sizeRect(boundary - baseMetrics.horizontalAdvance(size), ground.top(),
                             baseMetrics.horizontalAdvance(size), ground.height());
        painter->setPen(t.textTertiary);
        painter->drawText(sizeRect, Qt::AlignVCenter | Qt::AlignLeft, size);
        boundary -= sizeCost;
    }

    /* The band is right-aligned against whatever survives to its right, so it ends in one of two
     * places on the screen - at the kind chip's left edge, or at the row's own margin where the chip
     * used to be. It is never ragged within one of those two: tags are the field here that varies in
     * number, and a field that varies in number reads as a column when its edge is aligned. */
    boundary = paintTags(painter, ground, tags, cursor, boundary);

    /* The one weight difference in a row: a folder is somewhere you can go, so it carries the
     * demi-bold a Table gives its key column. */
    painter->setFont(m_theme->fontAt(t.fontBase, directory));
    QFontMetrics metrics(painter->font());
    painter->setPen(t.text);
    const QRect nameRect(cursor, ground.top(), qMax(0, boundary - cursor), ground.height());
    painter->drawText(nameRect, Qt::AlignVCenter | Qt::AlignLeft,
                      metrics.elidedText(name, Qt::ElideRight, nameRect.width()));

    painter->restore();
}

QRect RowDelegate::cursorRect(const QRect &ground) const {
    const Tokens &t = m_theme->tokens();
    return QRect(ground.left() + t.sm, ground.center().y() - kIconSide / 2, kIconSide, kIconSide);
}
