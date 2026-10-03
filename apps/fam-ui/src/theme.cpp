#include "theme.h"

#include <QApplication>
#include <QStyleHints>

#include <algorithm>

#include <cmath>

/* Alpha-composite `layer` over an opaque `background`. Exported: a hand-painted row needs the same
 * flattening the contrast audit uses, and two copies of that arithmetic is how a theme starts
 * reporting numbers its own pixels do not match. */
QColor flattenOnto(const QColor &layer, const QColor &background) {
    const double a = layer.alphaF();
    return QColor::fromRgbF(layer.redF() * a + background.redF() * (1.0 - a),
                            layer.greenF() * a + background.greenF() * (1.0 - a),
                            layer.blueF() * a + background.blueF() * (1.0 - a));
}

namespace {

/* Ant Design's neutral ramps are an opacity ladder over one base colour: 88 / 65 / 45 / 25 for text
 * and 15 / 6 / 4 / 2 for fills, over black in the light scheme and over white in the dark one.
 * Writing them as a ladder with the base as an argument is what lets the dark scheme be derived by
 * the same rule the library applies, rather than by whoever ported it remembering hex digits. */
QColor alpha(const QColor &base, double opacity) {
    QColor out = base;
    out.setAlphaF(opacity);
    return out;
}

double channel(double value) {
    return value <= 0.03928 ? value / 12.92 : std::pow((value + 0.055) / 1.055, 2.4);
}

double luminance(const QColor &color) {
    return 0.2126 * channel(color.redF()) + 0.7152 * channel(color.greenF()) +
           0.0722 * channel(color.blueF());
}

/* WCAG 2.x contrast ratio, always >= 1. */
double contrast(const QColor &raw, const QColor &background) {
    const QColor fg = flattenOnto(raw, background);
    const double first = luminance(fg);
    const double second = luminance(background);
    const double lighter = std::max(first, second);
    const double darker = std::min(first, second);
    return (lighter + 0.05) / (darker + 0.05);
}

/* Qt style sheets have no custom properties, so the sheet is written against token names and
 * substituted once per theme. `rgba()` is used for anything with alpha because Qt reads a
 * trailing byte in `#RRGGBBAA` as a different colour than CSS does. */
QString css(const QColor &color) {
    return QStringLiteral("rgba(%1, %2, %3, %4)")
        .arg(color.red())
        .arg(color.green())
        .arg(color.blue())
        .arg(qRound(color.alphaF() * 255.0));
}

const char *kSheet = R"QSS(
/* ---- base ------------------------------------------------------------
   Everything inherits type and text colour from here; no widget names a
   font size of its own. */
QWidget {
    color: {{text}};
    font-family: "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI",
                 "PingFang SC", "Noto Sans CJK SC", sans-serif;
    font-size: {{fontBase}}px;
}
QToolTip {
    color: {{onSpotlight}};
    background: {{spotlight}};
    border: none;
    padding: 6px 8px;
    border-radius: {{radius}}px;
}

/* ---- surfaces -------------------------------------------------------- */
QWidget#page { background: {{bgLayout}}; }
QWidget#toolbar {
    background: {{bgContainer}};
    border-bottom: 1px solid {{split}};
}
QWidget#column {
    background: {{bgContainer}};
    border: 1px solid {{borderSecondary}};
    border-radius: {{radiusLG}}px;
}
QListView#fileList, QListWidget {
    background: {{bgContainer}};
    border: none;
    padding: {{xxs}}px 0px;
    outline: none;
}
/* The "this column will take it" answer: while one of our own payloads is over a column, its list
 * grows the interaction colour as a border. The destination is the column's folder, not a row -
 * which is why the indicator is on the whole list and why the drop indicator line stayed off
 * (increment 14): a line between two rows says "lands here", and nothing lands between rows. */
QListView#fileList[dropHover="true"] {
    border: 1px solid {{primary}};
    border-radius: {{radiusLG}}px;
}

/* ---- rows ------------------------------------------------------------
   The file rows are painted by RowDelegate, which owns their ground, inset and radius: a delegate
   that paints its own text stops the view applying any `::item` rule, so none is written for them
   here - two places deciding how a selected row looks is how they start to disagree. What the style
   sheet does own is the drive rail, which is an ordinary list. */

/* ---- the drive rail: a Menu, not a list ------------------------------ */
QListWidget#sidebar { background: {{bgContainer}}; border-right: 1px solid {{split}}; }
QListWidget#sidebar::item {
    height: {{controlLG}}px;
    margin: 0px {{xs}}px;
    padding: 0px {{sm}}px;
    border: none;
    border-radius: {{radiusLG}}px;
    color: {{textSecondary}};
}
QListWidget#sidebar::item:hover { background: {{bgHover}}; color: {{text}}; }
QListWidget#sidebar::item:selected {
    background: {{bgSelected}};
    color: {{accentText}};
    font-weight: 600;
}
QListWidget#sidebar::item:disabled { color: {{textQuaternary}}; }

/* ---- inputs ----------------------------------------------------------
   The focus ring Ant Design draws as a 2px box-shadow becomes a 1px border
   of the same hue: Qt style sheets have no box-shadow, and widening the
   border would move the text by 2px, which is worse. */
QLineEdit, QComboBox {
    height: {{control}}px;
    padding: 0px {{controlPaddingInline}}px;
    background: {{bgContainer}};
    border: 1px solid {{border}};
    border-radius: {{radius}}px;
    color: {{text}};
    selection-background-color: {{primary}};
    selection-color: #ffffff;
}
QLineEdit:hover, QComboBox:hover { border-color: {{primaryHover}}; }
QLineEdit:focus, QComboBox:focus { border-color: {{primary}}; }
QLineEdit:disabled, QComboBox:disabled {
    color: {{textQuaternary}};
    background: {{fillAlter}};
    border-color: {{borderSecondary}};
}
QTextEdit#noteEditor {
    background: {{bgContainer}};
    border: 1px solid {{border}};
    border-radius: {{radius}}px;
    padding: {{sm}}px;
    color: {{text}};
    selection-background-color: {{primary}};
    selection-color: #ffffff;
}
QTextEdit#noteEditor:hover { border-color: {{primaryHover}}; }
QTextEdit#noteEditor:focus { border-color: {{primary}}; }
QLineEdit#searchInput {
    padding-left: {{md}}px;
    min-width: 260px;
}
QComboBox { padding-right: 26px; min-width: 96px; }
QComboBox::drop-down {
    subcontrol-origin: padding;
    subcontrol-position: center right;
    width: 22px;
    border: none;
    background: transparent;
}
QComboBox::down-arrow {
    width: 0px;
    height: 0px;
    border-style: solid;
    border-width: 4px 4px 0px 4px;
    border-color: {{textTertiary}} transparent transparent transparent;
    margin-top: 2px;
}
QComboBox QAbstractItemView {
    background: {{bgElevated}};
    border: 1px solid {{borderSecondary}};
    border-radius: {{radiusLG}}px;
    padding: {{xxs}}px;
    outline: none;
    selection-background-color: {{bgSelected}};
    selection-color: {{text}};
}

/* ---- buttons ---------------------------------------------------------
   `color` on hover is always `accentText`, never `primaryHover`: blue 5 as a border is fine, blue 5
   as 14px text on white is 2.99:1 and nobody could read it comfortably. */
QPushButton {
    height: {{control}}px;
    padding: 0px {{controlPaddingInline}}px;
    background: {{bgContainer}};
    border: 1px solid {{border}};
    border-radius: {{radius}}px;
    color: {{text}};
    font-weight: 400;
}
QPushButton:hover { color: {{accentText}}; border-color: {{primaryHover}}; }
QPushButton:pressed { color: {{accentText}}; border-color: {{primaryActive}}; }
QPushButton:disabled {
    color: {{textQuaternary}};
    border-color: {{borderSecondary}};
    background: {{fillAlter}};
}

/* A checkable button has to say it is checked. Ant Design's own answer is the selected chip: the
 * brand tint as a ground, the brand colour as the border and as the text. Without this the split
 * toggle looks identical pressed and unpressed, and a button whose state cannot be read is a button
 * that gets pressed twice. */
QPushButton#toggleButton { color: {{textSecondary}}; }
QPushButton#toggleButton:checked {
    color: {{accentText}};
    background: {{primaryBg}};
    border-color: {{primary}};
}
QPushButton#toggleButton:checked:hover { border-color: {{primaryHover}}; }
QPushButton#toggleButton:checked:pressed { border-color: {{primaryActive}}; }
/* The vocabulary strip's chips. Unchecked is an outline control in the metadata colour; checked is
 * exactly what a row's tag chip wears - the brand's preset Tag over the container - so the same
 * label reads as the same thing in both places, and the contrast pairs are the ones already
 * reported (标签文字/标签底 for the checked ground, 次级文字/容器 for the unchecked one). */
QPushButton#tagChip {
    height: {{control}}px;
    padding: 0px {{sm}}px;
    background: {{bgContainer}};
    border: 1px solid {{borderSecondary}};
    border-radius: {{radiusSM}}px;
    color: {{textSecondary}};
    font-weight: 400;
}
QPushButton#tagChip:hover { color: {{accentText}}; border-color: {{primaryHover}}; }
QPushButton#tagChip:checked {
    background: {{primaryBg}};
    border-color: {{tagBorder}};
    color: {{accentText}};
    font-weight: 500;
}
/* Exclude: the error family with a strike - "files carrying this are out". Read as a state, not
 * as a broken chip: the strike is the visual verb. */
QPushButton#tagChip[chipState="exclude"] {
    background: {{errorBg}};
    border-color: {{errorBorder}};
    color: {{errorText}};
    font-weight: 500;
    text-decoration: line-through;
}
QPushButton#tagChip[chipState="exclude"]:hover { border-color: {{errorText}}; }
QPushButton#tagGear {
    width: 32px;
    height: 32px;
    padding: 0px;
}
QPushButton#primaryButton {
    background: {{primarySolid}};
    border-color: {{primarySolid}};
    color: {{onPrimary}};
    font-weight: 500;
}
QPushButton#primaryButton:hover {
    background: {{primarySolidHover}};
    border-color: {{primarySolidHover}};
    color: {{onPrimary}};
}
QPushButton#primaryButton:pressed {
    background: {{primarySolidActive}};
    border-color: {{primarySolidActive}};
    color: {{onPrimary}};
}
QPushButton#primaryButton:disabled {
    background: {{primarySolid}};
    border-color: {{primarySolid}};
    color: {{onPrimary}};
}
/* The column header is one strip: the folder's name on the left in the metadata colour, the sort
 * control on the right as a Table header cell - 600 weight on the alter fill, a hairline under it,
 * and the interaction colour only where a pointer is. */
QWidget#columnHeader {
    background: {{fillAlter}};
    border-top-left-radius: {{radiusLG}}px;
    border-top-right-radius: {{radiusLG}}px;
    border-bottom: 1px solid {{split}};
}
QPushButton#sortHeader {
    height: 38px;
    padding: 0px {{sm}}px;
    background: transparent;
    border: none;
    border-radius: 0px;
    color: {{textSecondary}};
    font-weight: 600;
    text-align: right;
}
QPushButton#sortHeader:hover { color: {{accentText}}; }
QPushButton#sortHeader:pressed { color: {{accentText}}; }
QPushButton#iconButton {
    width: {{control}}px;
    height: {{control}}px;
    padding: 0px;
    border: 1px solid transparent;
    background: transparent;
    color: {{textSecondary}};
    border-radius: {{radius}}px;
}
QPushButton#iconButton:hover {
    color: {{accentText}};
    background: {{fillTertiary}};
    border-color: transparent;
}
QPushButton#iconButton:pressed { color: {{accentText}}; background: {{fillSecondary}}; }

/* ---- menus -----------------------------------------------------------
   The one popup this shell builds itself: a chip's right-click merge menu. Left to Fusion it would
   take the desktop's colours - the same way an unnamed QDialog did, which is the lesson the tag
   dialog's first screenshot taught. */
QMenu#tagChipMenu {
    background: {{bgElevated}};
    color: {{text}};
    border: 1px solid {{borderSecondary}};
    border-radius: {{radius}}px;
    padding: {{xxs}}px;
}
QMenu#tagChipMenu::item {
    padding: 4px 24px 4px 12px;
    border-radius: {{radiusSM}}px;
    color: {{text}};
}
QMenu#tagChipMenu::item:selected { background: {{bgSelected}}; color: {{accentText}}; }
QMenu#tagChipMenu::item:disabled { color: {{textQuaternary}}; }
QMenu#tagChipMenu::separator { height: 1px; background: {{split}}; margin: {{xxs}}px 0px; }

/* ---- labels ---------------------------------------------------------- */
QLabel { background: transparent; }
QLabel#statusLine { color: {{textSecondary}}; font-size: {{fontSM}}px; }
QLabel#columnPath {
    color: {{textTertiary}};
    font-size: {{fontSM}}px;
    padding: 0px {{sm}}px;
}
QLabel#emptyState { color: {{textTertiary}}; font-size: {{fontBase}}px; }

/* ---- scrollbars: Ant Design's overlay shape --------------------------
   12px track, 4px inset, fully rounded thumb, no arrow buttons. */
QScrollBar:vertical {
    width: 12px;
    margin: 0px;
    background: transparent;
}
QScrollBar::handle:vertical {
    background: {{scrollbar}};
    border-radius: 4px;
    min-height: 28px;
    margin: 4px 4px 4px 0px;
}
QScrollBar::handle:vertical:hover { background: {{scrollbarHover}}; }
QScrollBar:horizontal {
    height: 12px;
    margin: 0px;
    background: transparent;
}
QScrollBar::handle:horizontal {
    background: {{scrollbar}};
    border-radius: 4px;
    min-width: 28px;
    margin: 4px 0px 4px 4px;
}
QScrollBar::handle:horizontal:hover { background: {{scrollbarHover}}; }
QScrollBar::add-line, QScrollBar::sub-line { width: 0px; height: 0px; background: none; border: none; }
QScrollBar::add-page, QScrollBar::sub-page { background: transparent; }
)QSS";

} /* namespace */

Theme::Theme(Scheme scheme) {
    Tokens &t = m_tokens;
    t.scheme = scheme;

    /* Ant Design's default blue ramp: 4 / 5 / 6 / 7 of the palette named blue. */
    const QColor blue4(QStringLiteral("#69b1ff"));
    const QColor blue5(QStringLiteral("#4096ff"));
    const QColor blue6(QStringLiteral("#1677ff"));
    const QColor blue7(QStringLiteral("#0958d9"));

    if (scheme == Scheme::Light) {
        const QColor base = QColor(Qt::black);
        t.primary = blue6;
        t.primaryHover = blue5;
        t.primaryActive = blue7;
        t.primaryBg = QColor(QStringLiteral("#e6f4ff"));
        /* Blue 7 as the filled-button ground, and the hover goes *down* the ramp rather than up it
         * the way Ant Design's own button does: the only reason that button can hover to blue 5 is
         * that its label is white on blue 6, which is 4.10:1. Deeper keeps the label at 6.2:1 and
         * more. */
        t.primarySolid = blue7;
        t.primarySolidHover = blue7.darker(110);
        t.primarySolidActive = blue7.darker(130);
        t.accentText = blue7;
        /* Ant Design's `colorPrimaryBorder`, which in its own tag component is blue 3 over blue 0. The
         * row's tag chips need it because `bgSelected` is `primaryBg` in this scheme: on a selected
         * row a chip's ground and its row are the same colour, and the hairline is the only thing
         * left saying where the chip ends. */
        t.tagBorder = QColor(QStringLiteral("#91caff"));
        /* Ant Design 5's error family: colorErrorBorder #ffccc7, colorErrorBg #fff2f0; the text
         * takes red-7 #d4380d because plain #ff4d4f measures ~3.4:1 as 12px text on the bg. */
        t.errorText = QColor(QStringLiteral("#c5221f"));
        t.errorBg = QColor(QStringLiteral("#fff2f0"));
        t.errorBorder = QColor(QStringLiteral("#ffccc7"));
        t.onPrimary = QColor(QStringLiteral("#ffffff"));
        t.onSpotlight = QColor(QStringLiteral("#ffffff"));

        t.text = alpha(base, 0.88);
        t.textSecondary = alpha(base, 0.65);
        t.textTertiary = alpha(base, 0.45);
        t.textQuaternary = alpha(base, 0.25);

        t.bgLayout = QColor(QStringLiteral("#f5f5f5"));
        t.bgContainer = QColor(QStringLiteral("#ffffff"));
        t.bgElevated = QColor(QStringLiteral("#ffffff"));
        t.bgHover = QColor(QStringLiteral("#f5f5f5"));
        t.bgSelected = t.primaryBg;
        t.bgSpotlight = alpha(base, 0.85);
        t.border = QColor(QStringLiteral("#d9d9d9"));
        t.borderSecondary = QColor(QStringLiteral("#f0f0f0"));
        t.split = QColor(QStringLiteral("rgba(5, 5, 5, 0.06)"));

        t.fill = alpha(base, 0.15);
        t.fillSecondary = alpha(base, 0.06);
        t.fillTertiary = alpha(base, 0.04);
        t.fillAlter = alpha(base, 0.02);
        /* Ant Design's scrollbar is faint enough to fail the 3:1 it needs for a control a person has
         * to see in order to use. It is a control, so it gets the metadata colour. */
        t.scrollbar = alpha(base, 0.45);
        t.scrollbarHover = alpha(base, 0.60);
    } else {
        const QColor base = QColor(Qt::white);
        const QColor container(QStringLiteral("#141414"));
        t.primary = blue6;
        t.primaryHover = blue5;
        t.primaryActive = blue7;
        t.primaryBg = alpha(blue6, 0.15);
        /* On a dark ground the filled button goes the other way: blue 5 as the surface, with a
         * near-black label - 7.0:1 - where white on blue 5 would be 3.0:1. A button has to pick one
         * of {bright fill, dark fill} and the label follows it; here the fill is bright, so the
         * label is dark. */
        t.primarySolid = blue5;
        t.primarySolidHover = blue4;
        t.primarySolidActive = blue6;
        t.accentText = blue5;
        /* The dark mirror of the light hairline. Ant Design's own dark `colorPrimaryBorder` is a
         * desaturated dark blue that disappears on this scheme's `primaryBg` (a 15% brand fill over
         * near-black), so the hairline takes blue 4 at an alpha that puts it about two to one against
         * the chip's own ground - present, and not the loudest thing in the row. */
        t.tagBorder = alpha(blue4, 0.45);
        /* The dark error family: published errorBg #2c1618 flattened over the container, border
         * #5b2526 flattened likewise; the text takes the lighter error red because the dark
         * #dc4446 fails 4.5:1 at 12px. */
        t.errorText = QColor(QStringLiteral("#ff7875"));
        t.errorBg = flattenOnto(QColor(QStringLiteral("#2c1618")), t.bgContainer);
        t.errorBorder = flattenOnto(QColor(QStringLiteral("#5b2526")), t.bgContainer);
        t.onPrimary = alpha(QColor(Qt::black), 0.88);
        t.onSpotlight = alpha(QColor(Qt::black), 0.88);

        t.text = alpha(base, 0.88);
        t.textSecondary = alpha(base, 0.65);
        t.textTertiary = alpha(base, 0.45);
        t.textQuaternary = alpha(base, 0.25);

        t.bgLayout = QColor(QStringLiteral("#000000"));
        t.bgContainer = container;
        t.bgElevated = QColor(QStringLiteral("#1f1f1f"));
        t.bgHover = alpha(base, 0.08);
        t.bgSelected = t.primaryBg;
        t.bgSpotlight = alpha(base, 0.85);
        t.border = QColor(QStringLiteral("#424242"));
        t.borderSecondary = QColor(QStringLiteral("#303030"));
        /* The light scheme's split is a 6% hairline; the dark ramp's equivalent reads as no line at
         * all over #141414, so the divider takes the published secondary border instead of an
         * invented alpha. */
        t.split = t.borderSecondary;

        t.fill = alpha(base, 0.15);
        t.fillSecondary = alpha(base, 0.06);
        t.fillTertiary = alpha(base, 0.04);
        t.fillAlter = alpha(base, 0.04);
        t.scrollbar = alpha(base, 0.40);
        t.scrollbarHover = alpha(base, 0.55);
    }
    m_sheet = buildSheet();
}

Theme &Theme::instance() {
    /* Created against what the desktop is asking for, so a person who set a dark mode at the
     * operating-system level is not handed a white window on first run and then asked to fix it. */
    static Theme theme(systemScheme());
    return theme;
}

void Theme::setScheme(Scheme scheme) {
    /* Assigning a freshly built theme is cheaper and less error-prone than re-deriving the sheet
     * field by field: the constructor is the only place the token set is computed. */
    *this = Theme(scheme);
}

QFont Theme::fontAt(int pixelSize, bool bold) const {
    QFont font = QApplication::font();
    font.setPixelSize(pixelSize);
    font.setWeight(bold ? QFont::DemiBold : QFont::Normal);
    return font;
}

QString Theme::buildSheet() const {
    const Tokens &t = m_tokens;
    QString sheet = QString::fromUtf8(kSheet);
    /* A token the sheet references but the map does not carry would otherwise ship as the literal
     * `{{token}}` text - which a style sheet reports as a parse error, on stdout, per widget. */
    const QMap<QString, QString> values {
        {QStringLiteral("text"), css(t.text)},
        {QStringLiteral("fontBase"), QString::number(t.fontBase)},
        {QStringLiteral("fontSM"), QString::number(t.fontSM)},
        {QStringLiteral("radius"), QString::number(t.radius)},
        {QStringLiteral("radiusLG"), QString::number(t.radiusLG)},
        {QStringLiteral("rowHeight"), QString::number(t.rowHeight)},
        {QStringLiteral("control"), QString::number(t.control)},
        {QStringLiteral("controlLG"), QString::number(t.controlLG)},
        {QStringLiteral("controlPaddingInline"), QString::number(t.controlPaddingInline)},
        {QStringLiteral("xxs"), QString::number(t.xxs)},
        {QStringLiteral("xs"), QString::number(t.xs)},
        {QStringLiteral("sm"), QString::number(t.sm)},
        {QStringLiteral("md"), QString::number(t.md)},
        {QStringLiteral("bgLayout"), css(t.bgLayout)},
        {QStringLiteral("bgContainer"), css(t.bgContainer)},
        {QStringLiteral("bgElevated"), css(t.bgElevated)},
        {QStringLiteral("bgHover"), css(t.bgHover)},
        {QStringLiteral("bgSelected"), css(t.bgSelected)},
        {QStringLiteral("spotlight"), css(t.bgSpotlight)},
        {QStringLiteral("split"), css(t.split)},
        {QStringLiteral("border"), css(t.border)},
        {QStringLiteral("borderSecondary"), css(t.borderSecondary)},
        {QStringLiteral("fillSecondary"), css(t.fillSecondary)},
        {QStringLiteral("fillTertiary"), css(t.fillTertiary)},
        {QStringLiteral("fillAlter"), css(t.fillAlter)},
        {QStringLiteral("textSecondary"), css(t.textSecondary)},
        {QStringLiteral("textTertiary"), css(t.textTertiary)},
        {QStringLiteral("textQuaternary"), css(t.textQuaternary)},
        {QStringLiteral("accentText"), css(t.accentText)},
        {QStringLiteral("primary"), css(t.primary)},
        {QStringLiteral("primaryBg"), css(t.primaryBg)},
        {QStringLiteral("primaryHover"), css(t.primaryHover)},
        {QStringLiteral("primaryActive"), css(t.primaryActive)},
        {QStringLiteral("primarySolid"), css(t.primarySolid)},
        {QStringLiteral("primarySolidHover"), css(t.primarySolidHover)},
        {QStringLiteral("primarySolidActive"), css(t.primarySolidActive)},
        {QStringLiteral("tagBorder"), css(t.tagBorder)},
        {QStringLiteral("errorText"), css(t.errorText)},
        {QStringLiteral("errorBg"), css(t.errorBg)},
        {QStringLiteral("errorBorder"), css(t.errorBorder)},
        {QStringLiteral("onPrimary"), css(t.onPrimary)},
        {QStringLiteral("onSpotlight"), css(t.onSpotlight)},
        {QStringLiteral("scrollbar"), css(t.scrollbar)},
        {QStringLiteral("scrollbarHover"), css(t.scrollbarHover)},
        {QStringLiteral("radiusSM"), QString::number(t.radiusSM)},
    };
    for (auto entry = values.constBegin(); entry != values.constEnd(); ++entry) {
        sheet.replace(QStringLiteral("{{") + entry.key() + QStringLiteral("}}"), entry.value());
    }
    return sheet;
}

QVector<Theme::ContrastPair> Theme::contrastPairs() const {
    const Tokens &t = m_tokens;
    struct Raw {
        const char *label;
        QColor foreground;
        QColor background;
        double floor;
        bool exempt;
    };
    /* The note dot sits on whichever ground the row happens to be on - container, hover, or
     * selected - and the pair below is quoted at the least favourable of the three, computed here
     * rather than trusted: a marker that passed on the container and failed on the selected fill
     * would disappear exactly when a person is looking at it. */
    const QColor noteGrounds[] = {flattenOnto(t.bgContainer, t.bgContainer),
                                  flattenOnto(t.bgHover, t.bgContainer),
                                  flattenOnto(t.bgSelected, t.bgContainer)};
    QColor worstNoteGround = noteGrounds[0];
    double worstNoteRatio = 99.0;
    for (const QColor &ground : noteGrounds) {
        const double ratio = contrast(t.accentText, ground);
        if (ratio < worstNoteRatio) {
            worstNoteRatio = ratio;
            worstNoteGround = ground;
        }
    }
    /* The pairs are the ones the design actually puts on screen, not a sample of them: a text token
     * that appears in exactly one place is still a place someone has to read. */
    const QVector<Raw> raw {
        {"正文 / 容器", t.text, t.bgContainer, 4.5, false},
        {"正文 / 页面底", t.text, t.bgLayout, 4.5, false},
        {"次级文字 / 容器", t.textSecondary, t.bgContainer, 4.5, false},
        {"三级文字 / 容器", t.textTertiary, t.bgContainer, 3.0, false},
        {"禁用文字 / 容器", t.textQuaternary, t.bgContainer, 3.0, true},
        {"按钮文字 / 按钮底", t.onPrimary, t.primarySolid, 4.5, false},
        {"按钮文字 / 悬停底", t.onPrimary, t.primarySolidHover, 4.5, false},
        {"按钮文字 / 按下底", t.onPrimary, t.primarySolidActive, 4.5, false},
        {"强调文字 / 容器", t.accentText, t.bgContainer, 4.5, false},
        /* The tag chip a file row paints. The pair is the text against the chip's own flattened
         * ground rather than against the row, because the chip fills its ground opaquely - which is
         * also why the ratio holds on a selected row, where the chip's ground and the row's are the
         * same colour and only the hairline still says where the chip ends. */
        {"标签文字 / 标签底", t.accentText, flattenOnto(t.primaryBg, t.bgContainer), 4.5, false},
        {"标签描边 / 标签底", t.tagBorder, flattenOnto(t.primaryBg, t.bgContainer), 3.0, true},
        /* The filter strip's chip, unchecked: the same decorative hairline the tag pair above
         * excuses, over the container the strip sits on. The checked state is the tag pair itself. */
        {"筛选芯片描边 / 容器", t.borderSecondary, t.bgContainer, 3.0, true},
        {"选中行文字 / 选中底", t.text, flattenOnto(t.bgSelected, t.bgContainer), 4.5, false},
        {"悬停行文字 / 悬停底", t.text, flattenOnto(t.bgHover, t.bgContainer), 4.5, false},
        {"提示文字 / 气泡", t.onSpotlight,
         flattenOnto(t.bgSpotlight, t.bgElevated), 4.5, false},
        {"滚动条 / 容器", t.scrollbar, t.bgContainer, 3.0, false},
        {"表头文字 / 表头底", t.textSecondary, flattenOnto(t.fillAlter, t.bgContainer), 4.5, false},
        {"输入框边框 / 容器", t.border, t.bgContainer, 3.0, true},
        /* The merge menu's two readable pairs, on the elevated ground a popup floats on. */
        {"菜单文字 / 菜单底", t.text, t.bgElevated, 4.5, false},
        {"菜单选中文字 / 菜单选中底", t.accentText, flattenOnto(t.bgSelected, t.bgElevated), 4.5, false},
        /* The has-a-note dot, quoted at its worst ground (see above). A UI component that carries
         * information, so the non-text 3:1 floor applies rather than the decorative exemption. */
        {"备注点 / 最不利行底", t.accentText, worstNoteGround, 3.0, false},
        /* The drop hover's border against the container it sits on: an indicator that says "this
         * column will take it", which is information - so 3:1, gated, unlike the input border it
         * sits beside in the exempt list. */
        {"拖放描边 / 容器", t.primary, t.bgContainer, 3.0, false},
        /* The exclusion chip's two readable pairs, on the error ground the chip fills. */
        {"排除文字 / 排除底", t.errorText, flattenOnto(t.errorBg, t.bgContainer), 4.5, false},
        {"排除描边 / 排除底", t.errorBorder, flattenOnto(t.errorBg, t.bgContainer), 3.0, true},
    };
    QVector<ContrastPair> pairs;
    for (const Raw &entry : raw) {
        const double ratio = contrast(entry.foreground, entry.background);
        pairs.append({QString::fromUtf8(entry.label), ratio, entry.floor, entry.exempt,
                      entry.exempt || ratio >= entry.floor});
    }
    return pairs;
}

QString Theme::contrastReport() const {
    const QVector<ContrastPair> pairs = contrastPairs();
    QString report;
    int failures = 0;
    for (const ContrastPair &pair : pairs) {
        if (!pair.passes) {
            ++failures;
        }
        /* Fixed columns rather than tabs, because CJK labels are double-width where this gets read
         * and a tab stop lands wherever the previous line happened to end. */
        const QString name = pair.label + QString(qMax(1, 28 - pair.label.length() * 2), QChar(u' '));
        report += QStringLiteral("  %1 %2%3:1  %4\n")
                      .arg(name)
                      .arg(pair.exempt ? QStringLiteral("免除 ") : QString(7, QChar(u' ')))
                      .arg(pair.ratio, 0, 'f', 2)
                      .arg(pair.passes ? QStringLiteral("过") : QStringLiteral("不过"));
    }
    report += QStringLiteral("  门槛：正文 4.5:1，大字与非文字 3.0:1；受门槛的 %1 对里 %2 对不过\n")
                  .arg(std::count_if(pairs.begin(), pairs.end(),
                                     [](const ContrastPair &pair) { return !pair.exempt; }))
                  .arg(failures);
    return report;
}

double Theme::tightestRatio() const {
    double tightest = 99.0;
    for (const ContrastPair &pair : contrastPairs()) {
        if (pair.exempt) {
            continue;
        }
        tightest = std::min(tightest, pair.ratio);
    }
    return tightest;
}

Scheme Theme::systemScheme() {
    const auto hints = QApplication::styleHints();
    /* Scoped inside `Qt::ColorScheme` since Qt 6.8; before that the same three values hung off
     * `Qt::` directly. */
    switch (hints->colorScheme()) {
    case Qt::ColorScheme::Dark:
        return Scheme::Dark;
    case Qt::ColorScheme::Light:
    case Qt::ColorScheme::Unknown:
    default:
        return Scheme::Light;
    }
}
